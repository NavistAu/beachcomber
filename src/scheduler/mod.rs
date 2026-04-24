pub mod lifecycle;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::time::{Duration, interval};
use tracing::{debug, info, warn};

use crate::cache::Cache;
use crate::config::Config;
pub use crate::cache::FailureSnapshot;
use crate::provider::FieldScope;
use crate::provider::InvalidationStrategy;
use crate::provider::registry::ProviderRegistry;
use crate::scheduler::lifecycle::{
    LifecycleRegistry, LifecycleState, ProviderLifecycleConfig, StateTransition, WatchAction,
};
use crate::watcher::FsWatcher;
use crate::watcher_registry::WatcherRegistry;

/// Messages sent from the Server to the Scheduler.
#[derive(Debug)]
pub enum SchedulerMessage {
    Refresh {
        provider: String,
        path: Option<String>,
    },
    FsEvent {
        paths: Vec<PathBuf>,
    },
    Shutdown,
    /// Request scheduler status info. Response sent via oneshot channel.
    GetStatus {
        reply: tokio::sync::oneshot::Sender<SchedulerStatus>,
    },
    /// A provider+path was queried via get. Signals demand to keep it warm.
    QueryActivity {
        provider: String,
        path: Option<String>,
    },
    /// Request a snapshot of all (provider, path) entries with non-zero failure counts.
    /// Returns an empty map when no failures are tracked.
    GetFailureStates {
        reply: tokio::sync::oneshot::Sender<HashMap<lifecycle::Key, FailureSnapshot>>,
    },
    /// Request a full per-entry snapshot of the lifecycle registry.
    /// Used by the status response handler to annotate CacheRows with lifecycle info.
    GetLifecycleSnapshots {
        reply: tokio::sync::oneshot::Sender<HashMap<lifecycle::Key, LifecycleSnapshot>>,
    },
}

/// A point-in-time snapshot of a single lifecycle registry entry.
#[derive(Debug, Clone)]
pub struct LifecycleSnapshot {
    /// 0 = Active, 1–4 = Decay step.
    pub decay: u8,
    /// Effective poll interval in seconds (may be doubled during decay).
    pub poll_interval_secs: u64,
    /// Number of keep-alive polls before decay begins.
    pub keep_alive_polls: u32,
    /// Whether fsevents are reinstated on demand for this entry.
    pub fsevents_reinstate: bool,
    /// True if the provider uses Watch or WatchAndPoll invalidation strategy.
    pub watches_files: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SchedulerStatus {
    pub watched_paths: Vec<String>,
    pub in_flight: Vec<String>,
    pub lifecycle: Vec<LifecycleInfo>,
    pub poll_timers: Vec<PollTimerInfo>,
    pub demand: Vec<DemandInfo>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DemandInfo {
    pub provider: String,
    pub path: Option<String>,
    pub last_query_secs_ago: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LifecycleInfo {
    pub provider: String,
    pub path: Option<String>,
    pub stage: String,
    pub elapsed_secs: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PollTimerInfo {
    pub provider: String,
    pub path: Option<String>,
    pub interval_secs: u64,
    pub last_run_secs_ago: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct Subscription {
    pub(crate) provider: String,
    pub(crate) path: Option<String>,
    pub(crate) patterns: Vec<String>,
}

/// Public wrapper for parse_duration, used by script provider.
pub fn parse_duration_secs_pub(s: &str) -> Option<u64> {
    crate::config::parse_duration(s).map(|d| d.as_secs())
}

/// Handle for sending messages to the scheduler.
#[derive(Clone)]
pub struct SchedulerHandle {
    tx: mpsc::Sender<SchedulerMessage>,
}

impl SchedulerHandle {
    pub fn new(tx: mpsc::Sender<SchedulerMessage>) -> Self {
        Self { tx }
    }

    pub async fn send(&self, msg: SchedulerMessage) {
        let _ = self.tx.send(msg).await;
    }

    pub async fn get_status(&self) -> Option<SchedulerStatus> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(SchedulerMessage::GetStatus { reply: reply_tx })
            .await
            .ok()?;
        reply_rx.await.ok()
    }

    /// Return a snapshot of all (provider, path) entries with non-zero consecutive failure counts.
    /// Returns an empty map when no failures are tracked.
    pub async fn get_failure_states(&self) -> HashMap<lifecycle::Key, FailureSnapshot> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = self
            .tx
            .send(SchedulerMessage::GetFailureStates { reply: tx })
            .await;
        rx.await.unwrap_or_default()
    }

    /// Return a full per-entry snapshot of every lifecycle-tracked key.
    /// Missing keys are not in the lifecycle registry (e.g. virtual/put entries).
    pub async fn get_lifecycle_snapshots(
        &self,
    ) -> HashMap<lifecycle::Key, LifecycleSnapshot> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = self
            .tx
            .send(SchedulerMessage::GetLifecycleSnapshots { reply: tx })
            .await;
        rx.await.unwrap_or_default()
    }
}

/// Tracks consecutive failures and suppression state for a provider key.
struct FailureState {
    consecutive_failures: u32,
    suppressed_until: Option<Instant>,
    threshold: u32,
    backoff_interval: Duration,
}

impl FailureState {
    fn new(threshold: u32, backoff_interval: Duration) -> Self {
        Self {
            consecutive_failures: 0,
            suppressed_until: None,
            threshold,
            backoff_interval,
        }
    }

    fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= self.threshold {
            // 4 levels of exponential backoff from base interval, stays at level 4
            let level = (self.consecutive_failures - self.threshold).min(3);
            let delay = self.backoff_interval * (1u32 << level);
            self.suppressed_until = Some(Instant::now() + delay);
        }
    }

    fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.suppressed_until = None;
    }

    fn is_suppressed(&self) -> bool {
        self.suppressed_until
            .map(|until| Instant::now() < until)
            .unwrap_or(false)
    }
}

/// Convert a future `Instant` deadline into a Unix-millisecond timestamp by
/// anchoring against the current monotonic and wall clocks. Past deadlines
/// resolve to "now" (saturating).
fn instant_to_unix_ms(deadline: std::time::Instant) -> u64 {
    let now_inst = std::time::Instant::now();
    let now_sys = std::time::SystemTime::now();
    let remaining = deadline.saturating_duration_since(now_inst);
    let absolute = now_sys + remaining;
    absolute
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Build a snapshot of the current scheduler state for status reporting.
fn build_status(
    lifecycle: &LifecycleRegistry,
    watch_paths: &HashMap<PathBuf, Vec<Subscription>>,
    in_flight: &std::sync::Mutex<std::collections::HashSet<(String, Option<String>)>>,
) -> SchedulerStatus {
    let watched: Vec<String> = watch_paths
        .keys()
        .map(|p| p.to_string_lossy().to_string())
        .collect();

    let in_flight_keys: Vec<String> = in_flight
        .lock()
        .unwrap()
        .iter()
        .map(|(p, path)| match path {
            Some(pa) => format!("{p}:{pa}"),
            None => p.clone(),
        })
        .collect();

    let mut backoff_info: Vec<LifecycleInfo> = Vec::new();
    let mut poll_timer_info: Vec<PollTimerInfo> = Vec::new();
    let mut demand_info: Vec<DemandInfo> = Vec::new();

    for ((provider, path), entry) in lifecycle.iter() {
        // Poll timer info for all entries.
        poll_timer_info.push(PollTimerInfo {
            provider: provider.clone(),
            path: path.clone(),
            interval_secs: entry.poll_timer.interval.as_secs(),
            last_run_secs_ago: entry.poll_timer.last_fired.elapsed().as_secs(),
        });

        match entry.state {
            LifecycleState::Active => {
                // Active entries have demand — report last_demand time.
                demand_info.push(DemandInfo {
                    provider: provider.clone(),
                    path: path.clone(),
                    last_query_secs_ago: entry.decay_timer.last_demand.elapsed().as_secs(),
                });
            }
            LifecycleState::Decay(step) => {
                // Decaying entries go in the lifecycle list.
                backoff_info.push(LifecycleInfo {
                    provider: provider.clone(),
                    path: path.clone(),
                    stage: format!("Decay{}", step.as_u8()),
                    elapsed_secs: entry.decay_timer.last_demand.elapsed().as_secs(),
                });
            }
        }
    }

    SchedulerStatus {
        watched_paths: watched,
        in_flight: in_flight_keys,
        lifecycle: backoff_info,
        poll_timers: poll_timer_info,
        demand: demand_info,
    }
}

type ProviderKeySet = Arc<std::sync::Mutex<std::collections::HashSet<(String, Option<String>)>>>;
type ProviderFailureMap = Arc<std::sync::Mutex<HashMap<(String, Option<String>), FailureState>>>;

/// Returns true if any component of the event path (relative to the watched root)
/// equals any of the patterns. Matching happens at ANY depth — an event at
/// `some/nested/.git/HEAD` matches the pattern `.git`. This is intentional for
/// monorepos with nested git submodules.
///
/// If the event path IS the root, the root's basename is matched instead.
///
/// Empty patterns are fail-open (every event matches) — preserves behaviour for
/// providers that haven't declared patterns. Returns false if event_path is not
/// under root.
pub(crate) fn event_matches_patterns(
    patterns: &[String],
    root: &std::path::Path,
    event_path: &std::path::Path,
) -> bool {
    if patterns.is_empty() {
        return true;
    }
    let Ok(relative) = event_path.strip_prefix(root) else {
        return false;
    };
    // If event fires on the root itself, match against the root's basename.
    if relative.as_os_str().is_empty() {
        if let Some(name) = event_path.file_name() {
            let name_str = name.to_string_lossy();
            return patterns.iter().any(|p| p == name_str.as_ref());
        }
        return false;
    }
    relative.components().any(|c| {
        let name = c.as_os_str().to_string_lossy();
        patterns.iter().any(|p| p == name.as_ref())
    })
}

/// The scheduler core loop: executes providers on demand and manages subscriptions.
pub struct Scheduler {
    cache: Arc<Cache>,
    registry: Arc<ProviderRegistry>,
    config: Config,
    rx: mpsc::Receiver<SchedulerMessage>,
    /// Tracks which (provider, path) combinations are currently executing.
    in_flight: ProviderKeySet,
    /// Tracks which (provider, path) need to re-run after current execution completes.
    pending_rerun: ProviderKeySet,
    /// Tracks consecutive failures and suppression state per (provider, path).
    failure_counts: ProviderFailureMap,
    /// Monotonically increasing counter bumped on every tick. Used by the watchdog
    /// to detect scheduler stalls.
    heartbeat: Arc<AtomicU64>,
    /// WatcherRegistry — gc() called periodically to remove dead channel entries.
    watchers: Arc<WatcherRegistry>,
}

impl Scheduler {
    pub fn new(
        cache: Arc<Cache>,
        registry: Arc<ProviderRegistry>,
        config: Config,
        watchers: Arc<WatcherRegistry>,
    ) -> (SchedulerHandle, Scheduler) {
        let (tx, rx) = mpsc::channel(256);
        let handle = SchedulerHandle::new(tx);
        let heartbeat = Arc::new(AtomicU64::new(0));
        let scheduler = Scheduler {
            cache,
            registry,
            config,
            rx,
            in_flight: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            pending_rerun: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            failure_counts: Arc::new(std::sync::Mutex::new(HashMap::new())),
            heartbeat,
            watchers,
        };
        (handle, scheduler)
    }

    /// Returns a clone of the heartbeat counter for external monitoring (watchdog).
    pub fn heartbeat(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.heartbeat)
    }

    /// Build the GC tick interval with the immediate first tick consumed.
    /// The first production GC runs ~60s after startup, not at startup.
    async fn gc_tick_for_prod() -> tokio::time::Interval {
        let mut t = tokio::time::interval(Duration::from_secs(60));
        t.tick().await;
        t
    }

    /// Execute a provider on the blocking thread pool and write result to cache.
    /// This is fire-and-forget: returns immediately while the provider runs in the background.
    /// Deduplicates concurrent executions: if a provider is already running, marks it for
    /// a single rerun after completion rather than launching another concurrent execution.
    /// Suppresses execution when failure backoff is active.
    fn execute_provider(&self, provider_name: &str, path: Option<&str>) {
        let Some(provider) = self.registry.get(provider_name) else {
            warn!("Refresh for unknown provider '{}'", provider_name);
            return;
        };

        let key = (provider_name.to_string(), path.map(|s| s.to_string()));

        // Check failure backoff — skip if suppressed.
        {
            let failures = self.failure_counts.lock().unwrap();
            if let Some(state) = failures.get(&key)
                && state.is_suppressed()
            {
                debug!(
                    "Provider '{}' suppressed due to failure backoff",
                    provider_name
                );
                return;
            }
        }

        // Check if already in flight — if so, queue a rerun and return.
        {
            let mut in_flight = self.in_flight.lock().unwrap();
            if in_flight.contains(&key) {
                self.pending_rerun.lock().unwrap().insert(key);
                debug!(
                    "Provider '{}' already in flight, queued rerun",
                    provider_name
                );
                return;
            }
            in_flight.insert(key.clone());
        }

        let path_owned = key.1.clone();
        let name_owned = key.0.clone();
        let cache = Arc::clone(&self.cache);
        let timeout_secs = self.config.daemon.provider_timeout_secs.unwrap_or(10);
        let in_flight = Arc::clone(&self.in_flight);
        let pending_rerun = Arc::clone(&self.pending_rerun);
        let registry = Arc::clone(&self.registry);
        let failure_counts = Arc::clone(&self.failure_counts);

        // Extract the poll interval from provider metadata for staleness tracking.
        let poll_interval_secs: Option<u64> =
            registry
                .get(provider_name)
                .and_then(|p| match p.metadata().invalidation {
                    InvalidationStrategy::Poll { interval_secs, .. } => Some(interval_secs),
                    InvalidationStrategy::WatchAndPoll { interval_secs, .. } => Some(interval_secs),
                    _ => None,
                });

        let path_for_cache = path_owned.clone();
        let name_for_log = name_owned.clone();
        let key_for_cleanup = key.clone();

        let failure_threshold = self.config.resolve_failure_reattempts(provider_name);
        let failure_backoff = self.config.resolve_failure_backoff_interval(provider_name);

        tokio::spawn(async move {
            let results = tokio::time::timeout(
                Duration::from_secs(timeout_secs),
                tokio::task::spawn_blocking(move || provider.execute(path_owned.as_deref())),
            )
            .await;

            // Record success or failure for backoff tracking.
            match &results {
                Ok(Ok(v)) if !v.is_empty() => {
                    failure_counts
                        .lock()
                        .unwrap()
                        .entry(key_for_cleanup.clone())
                        .or_insert_with(|| FailureState::new(failure_threshold, failure_backoff))
                        .record_success();
                }
                _ => {
                    failure_counts
                        .lock()
                        .unwrap()
                        .entry(key_for_cleanup.clone())
                        .or_insert_with(|| FailureState::new(failure_threshold, failure_backoff))
                        .record_failure();
                }
            }

            match results {
                Ok(Ok(provider_results)) => {
                    if provider_results.is_empty() {
                        debug!(
                            "Provider '{}' returned empty results for path={:?}",
                            name_for_log, path_for_cache
                        );
                    } else {
                        for (scope_path, provider_result) in provider_results {
                            cache.put_with_interval(
                                &name_owned,
                                scope_path.as_deref(),
                                provider_result,
                                poll_interval_secs,
                            );
                        }
                        debug!(
                            "Executed provider '{}' path={:?}",
                            name_owned, path_for_cache
                        );
                    }
                }
                Ok(Err(e)) => {
                    warn!("Provider '{}' panicked: {}", name_for_log, e);
                }
                Err(_) => {
                    warn!(
                        "Provider '{}' timed out after {}s",
                        name_for_log, timeout_secs
                    );
                }
            }

            // Clear in-flight and check for pending reruns.
            in_flight.lock().unwrap().remove(&key_for_cleanup);
            let should_rerun = pending_rerun.lock().unwrap().remove(&key_for_cleanup);

            if should_rerun {
                debug!(
                    "Re-running provider '{}' (was queued during previous execution)",
                    key_for_cleanup.0
                );
                if let Some(rerun_provider) = registry.get(&key_for_cleanup.0) {
                    let rerun_path = key_for_cleanup.1.clone();
                    let rerun_name = key_for_cleanup.0.clone();
                    let rerun_interval = crate::provider::expected_interval_secs(
                        &rerun_provider.metadata().invalidation,
                    );
                    // Mark as in-flight again for this rerun.
                    in_flight.lock().unwrap().insert(key_for_cleanup.clone());
                    tokio::spawn(async move {
                        let rerun_results = tokio::time::timeout(
                            Duration::from_secs(timeout_secs),
                            tokio::task::spawn_blocking(move || {
                                rerun_provider.execute(rerun_path.as_deref())
                            }),
                        )
                        .await;

                        // Record success or failure for the rerun.
                        match &rerun_results {
                            Ok(Ok(v)) if !v.is_empty() => {
                                failure_counts
                                    .lock()
                                    .unwrap()
                                    .entry(key_for_cleanup.clone())
                                    .or_insert_with(|| {
                                        FailureState::new(failure_threshold, failure_backoff)
                                    })
                                    .record_success();
                            }
                            _ => {
                                failure_counts
                                    .lock()
                                    .unwrap()
                                    .entry(key_for_cleanup.clone())
                                    .or_insert_with(|| {
                                        FailureState::new(failure_threshold, failure_backoff)
                                    })
                                    .record_failure();
                            }
                        }

                        match rerun_results {
                            Ok(Ok(r)) => {
                                if r.is_empty() {
                                    debug!(
                                        "Rerun provider '{}' returned empty results",
                                        rerun_name
                                    );
                                } else {
                                    for (scope_path, result) in r {
                                        cache.put_with_interval(
                                            &rerun_name,
                                            scope_path.as_deref(),
                                            result,
                                            rerun_interval,
                                        );
                                    }
                                    debug!("Rerun provider '{}' completed", rerun_name);
                                }
                            }
                            Ok(Err(e)) => {
                                warn!("Rerun provider '{}' panicked: {}", rerun_name, e);
                            }
                            Err(_) => {
                                warn!(
                                    "Rerun provider '{}' timed out after {}s",
                                    rerun_name, timeout_secs
                                );
                            }
                        }
                        in_flight.lock().unwrap().remove(&key_for_cleanup);
                    });
                }
            }
        });
    }

    pub async fn run(mut self) {
        // Set up filesystem watcher.
        let (mut fs_watcher, mut fs_rx) = match FsWatcher::new() {
            Ok(pair) => pair,
            Err(e) => {
                warn!(
                    "Failed to create filesystem watcher: {}. Watch triggers disabled.",
                    e
                );
                // Create a channel that never receives to allow the rest of the loop to work.
                let (_tx, rx) = mpsc::channel::<Vec<PathBuf>>(1);
                // We can't easily create a no-op FsWatcher, so we'll handle this differently.
                // For now, proceed without watching support.
                drop(e);
                return self.run_without_watcher(rx).await;
            }
        };

        // Compute Once providers at startup.
        self.compute_once_providers();

        // Lifecycle registry: replaces the three old maps (demand, backoff, poll_states).
        let mut lifecycle = LifecycleRegistry::new();

        // Watch paths that are being monitored: path -> subscriptions
        let mut watch_paths: HashMap<PathBuf, Vec<Subscription>> = HashMap::new();

        // Idle shutdown tracking.
        let mut last_activity = Instant::now();
        let idle_shutdown_secs = self.config.lifecycle.idle_shutdown_secs;

        // Tick every second to check poll timers.
        let mut tick = interval(Duration::from_secs(1));

        // Periodic GC tick to remove dead channel entries from WatcherRegistry.
        let mut gc_tick = Self::gc_tick_for_prod().await;

        loop {
            tokio::select! {
                // Scheduler messages from server.
                msg = self.rx.recv() => {
                    match msg {
                        None => {
                            info!("Scheduler channel closed, shutting down.");
                            break;
                        }
                        Some(SchedulerMessage::Shutdown) => {
                            info!("Scheduler shutting down.");
                            break;
                        }
                        Some(SchedulerMessage::Refresh { provider, path }) => {
                            debug!("Refresh: provider={} path={:?}", provider, path);
                            self.execute_provider(&provider, path.as_deref());
                            last_activity = Instant::now();
                        }
                        Some(SchedulerMessage::FsEvent { paths }) => {
                            self.handle_fs_event(paths, &watch_paths);
                            last_activity = Instant::now();
                        }
                        Some(SchedulerMessage::GetStatus { reply }) => {
                            let status = build_status(&lifecycle, &watch_paths, &self.in_flight);
                            let _ = reply.send(status);
                        }
                        Some(SchedulerMessage::GetFailureStates { reply }) => {
                            let snap: HashMap<lifecycle::Key, FailureSnapshot> = self
                                .failure_counts
                                .lock()
                                .unwrap()
                                .iter()
                                .filter_map(|(key, state)| {
                                    if state.consecutive_failures == 0 {
                                        return None;
                                    }
                                    Some((
                                        key.clone(),
                                        FailureSnapshot {
                                            consecutive_failures: state.consecutive_failures,
                                            suppressed_until_unix_ms: state
                                                .suppressed_until
                                                .map(instant_to_unix_ms),
                                        },
                                    ))
                                })
                                .collect();
                            let _ = reply.send(snap);
                        }
                        Some(SchedulerMessage::GetLifecycleSnapshots { reply }) => {
                            let map = lifecycle
                                .iter()
                                .map(|(k, entry)| {
                                    let watches_files = self
                                        .registry
                                        .get(&k.0)
                                        .map(|p| {
                                            matches!(
                                                p.metadata().invalidation,
                                                InvalidationStrategy::Watch { .. }
                                                    | InvalidationStrategy::WatchAndPoll { .. }
                                            )
                                        })
                                        .unwrap_or(false);
                                    let snap = LifecycleSnapshot {
                                        decay: lifecycle::to_decay_level(&entry.state),
                                        poll_interval_secs: entry.poll_timer.interval.as_secs(),
                                        keep_alive_polls: entry.config.keep_alive_polls,
                                        fsevents_reinstate: entry.config.fsevents_reinstate,
                                        watches_files,
                                    };
                                    (k.clone(), snap)
                                })
                                .collect();
                            let _ = reply.send(map);
                        }
                        Some(SchedulerMessage::QueryActivity { provider, path }) => {
                            // Once providers don't participate in the lifecycle.
                            // They're populated once by compute_once_providers() at
                            // startup and never re-executed. Short-circuit before
                            // any lifecycle registration or watch setup.
                            let is_once = self
                                .registry
                                .get(&provider)
                                .map(|p| {
                                    matches!(
                                        p.metadata().invalidation,
                                        InvalidationStrategy::Once
                                    )
                                })
                                .unwrap_or(false);
                            if is_once {
                                last_activity = Instant::now();
                                continue;
                            }

                            let cfg = ProviderLifecycleConfig {
                                poll_interval: self.resolve_poll_interval_for(&provider),
                                keep_alive_polls: self.config.resolve_poll_live_count(&provider),
                                fsevents_reinstate: self.config.resolve_fsevents_reinstate(&provider),
                            };
                            let key = (provider.clone(), path.clone());
                            let outcome = lifecycle.on_demand(key.clone(), cfg, Instant::now());

                            match outcome.watch_registration {
                                WatchAction::Register | WatchAction::Reinstate => {
                                    // Register fs watches for path-scoped providers.
                                    if let Some(prov) = self.registry.get(&provider) {
                                        let meta = prov.metadata();
                                        if meta.inferred_scope() == FieldScope::PathScoped
                                            && let Some(ref path_str) = path
                                        {
                                            let watch_path = PathBuf::from(path_str);
                                            let patterns = crate::provider::watch_patterns(
                                                &meta.invalidation,
                                            );
                                            if let Err(e) = fs_watcher.watch(&watch_path) {
                                                warn!("Failed to watch {:?}: {}", watch_path, e);
                                            } else {
                                                watch_paths
                                                    .entry(watch_path)
                                                    .or_default()
                                                    .push(Subscription {
                                                        provider: provider.clone(),
                                                        path: path.clone(),
                                                        patterns,
                                                    });
                                                debug!(
                                                    "Demand: watching path {:?} for provider={}",
                                                    path, provider
                                                );
                                            }
                                        }
                                    }
                                }
                                WatchAction::Preserve => {
                                    // Watches are already live. Nothing to do.
                                }
                            }

                            if matches!(outcome.transition, StateTransition::NewlyActive) {
                                // Cold → Active: execute inline to populate cache.
                                if self.cache.get(&provider, path.as_deref()).is_none() {
                                    self.execute_provider(&provider, path.as_deref());
                                }
                            }
                            last_activity = Instant::now();
                        }
                    }
                }

                // Filesystem events from watcher.
                Some(paths) = fs_rx.recv() => {
                    let affected_keys = resolve_keys_from_paths(&paths, &watch_paths);
                    for key in affected_keys {
                        let outcome = lifecycle.on_fsevent(key.clone(), Instant::now());
                        if outcome.refresh {
                            self.execute_provider(&key.0, key.1.as_deref());
                        }
                    }
                    last_activity = Instant::now();
                }

                // Periodic GC: remove dead broadcast channel entries.
                _ = gc_tick.tick() => {
                    self.watchers.gc();
                }

                // Poll tick — check which subscriptions are due.
                _ = tick.tick() => {
                    self.heartbeat.fetch_add(1, Ordering::Relaxed);
                    let actions = lifecycle.tick(Instant::now());

                    for key in &actions.polls_due {
                        debug!("Poll tick: executing provider={} path={:?}", key.0, key.1);
                        self.execute_provider(&key.0, key.1.as_deref());
                    }

                    for key in &actions.watch_drops {
                        drop_watches_for_key(key, &mut watch_paths, &mut fs_watcher);
                    }

                    for key in &actions.evictions {
                        debug!("Evicting cache for provider={} path={:?}", key.0, key.1);
                        self.cache.remove(&key.0, key.1.as_deref());
                        drop_watches_for_key(key, &mut watch_paths, &mut fs_watcher);
                    }

                    for (key, new_state) in &actions.transitions {
                        debug!("lifecycle: key {:?} -> {:?}", key, new_state);
                    }

                    // Idle shutdown.
                    if let Some(idle_secs) = idle_shutdown_secs
                        && self.cache.is_empty()
                        && lifecycle.is_empty()
                        && last_activity.elapsed().as_secs() >= idle_secs
                    {
                        info!("Idle shutdown: no entries, idle for {}s", idle_secs);
                        break;
                    }
                }
            }
        }
    }

    /// Fallback run loop when FsWatcher creation fails — no watch support.
    async fn run_without_watcher(mut self, mut _dummy_rx: mpsc::Receiver<Vec<PathBuf>>) {
        self.compute_once_providers();

        // Lifecycle registry: replaces the three old maps (demand, backoff, poll_states).
        let mut lifecycle = LifecycleRegistry::new();

        let mut tick = interval(Duration::from_secs(1));

        // Periodic GC tick to remove dead channel entries from WatcherRegistry.
        let mut gc_tick = Self::gc_tick_for_prod().await;

        // Idle shutdown tracking.
        let mut last_activity = Instant::now();
        let idle_shutdown_secs = self.config.lifecycle.idle_shutdown_secs;

        loop {
            tokio::select! {
                msg = self.rx.recv() => {
                    match msg {
                        None | Some(SchedulerMessage::Shutdown) => break,
                        Some(SchedulerMessage::Refresh { provider, path }) => {
                            self.execute_provider(&provider, path.as_deref());
                            last_activity = Instant::now();
                        }
                        Some(SchedulerMessage::FsEvent { .. }) => {
                            // No-op without watcher.
                        }
                        Some(SchedulerMessage::GetStatus { reply }) => {
                            let empty_watch_paths: HashMap<PathBuf, Vec<Subscription>> = HashMap::new();
                            let status = build_status(&lifecycle, &empty_watch_paths, &self.in_flight);
                            let _ = reply.send(status);
                        }
                        Some(SchedulerMessage::GetFailureStates { reply }) => {
                            let snap: HashMap<lifecycle::Key, FailureSnapshot> = self
                                .failure_counts
                                .lock()
                                .unwrap()
                                .iter()
                                .filter_map(|(key, state)| {
                                    if state.consecutive_failures == 0 {
                                        return None;
                                    }
                                    Some((
                                        key.clone(),
                                        FailureSnapshot {
                                            consecutive_failures: state.consecutive_failures,
                                            suppressed_until_unix_ms: state
                                                .suppressed_until
                                                .map(instant_to_unix_ms),
                                        },
                                    ))
                                })
                                .collect();
                            let _ = reply.send(snap);
                        }
                        Some(SchedulerMessage::GetLifecycleSnapshots { reply }) => {
                            let map = lifecycle
                                .iter()
                                .map(|(k, entry)| {
                                    let watches_files = self
                                        .registry
                                        .get(&k.0)
                                        .map(|p| {
                                            matches!(
                                                p.metadata().invalidation,
                                                InvalidationStrategy::Watch { .. }
                                                    | InvalidationStrategy::WatchAndPoll { .. }
                                            )
                                        })
                                        .unwrap_or(false);
                                    let snap = LifecycleSnapshot {
                                        decay: lifecycle::to_decay_level(&entry.state),
                                        poll_interval_secs: entry.poll_timer.interval.as_secs(),
                                        keep_alive_polls: entry.config.keep_alive_polls,
                                        fsevents_reinstate: entry.config.fsevents_reinstate,
                                        watches_files,
                                    };
                                    (k.clone(), snap)
                                })
                                .collect();
                            let _ = reply.send(map);
                        }
                        Some(SchedulerMessage::QueryActivity { provider, path }) => {
                            // Once providers don't participate in the lifecycle.
                            let is_once = self
                                .registry
                                .get(&provider)
                                .map(|p| {
                                    matches!(
                                        p.metadata().invalidation,
                                        InvalidationStrategy::Once
                                    )
                                })
                                .unwrap_or(false);
                            if is_once {
                                last_activity = Instant::now();
                                continue;
                            }

                            let cfg = ProviderLifecycleConfig {
                                poll_interval: self.resolve_poll_interval_for(&provider),
                                keep_alive_polls: self.config.resolve_poll_live_count(&provider),
                                fsevents_reinstate: self.config.resolve_fsevents_reinstate(&provider),
                            };
                            let key = (provider.clone(), path.clone());
                            let outcome = lifecycle.on_demand(key.clone(), cfg, Instant::now());

                            // No filesystem watching in this path.
                            let _ = outcome.watch_registration;

                            if matches!(outcome.transition, StateTransition::NewlyActive) {
                                // Cold → Active: execute inline to populate cache.
                                if self.cache.get(&provider, path.as_deref()).is_none() {
                                    self.execute_provider(&provider, path.as_deref());
                                }
                            }
                            last_activity = Instant::now();
                        }
                    }
                }
                // Periodic GC: remove dead broadcast channel entries.
                _ = gc_tick.tick() => {
                    self.watchers.gc();
                }
                _ = tick.tick() => {
                    self.heartbeat.fetch_add(1, Ordering::Relaxed);
                    let actions = lifecycle.tick(Instant::now());

                    for key in &actions.polls_due {
                        self.execute_provider(&key.0, key.1.as_deref());
                    }

                    // No watches to drop in the no-watcher path.

                    for key in &actions.evictions {
                        debug!("Evicting cache for provider={} path={:?}", key.0, key.1);
                        self.cache.remove(&key.0, key.1.as_deref());
                    }

                    for (key, new_state) in &actions.transitions {
                        debug!("lifecycle: key {:?} -> {:?}", key, new_state);
                    }

                    // Idle shutdown.
                    if let Some(idle_secs) = idle_shutdown_secs
                        && self.cache.is_empty()
                        && lifecycle.is_empty()
                        && last_activity.elapsed().as_secs() >= idle_secs
                    {
                        info!("Idle shutdown: no entries, idle for {}s", idle_secs);
                        break;
                    }
                }
            }
        }
    }

    /// Resolve the effective poll interval for a provider, using the provider's
    /// own metadata as the fallback when no config override is present.
    /// This matches the old scheduler behaviour: metadata interval wins unless
    /// the provider appears in [providers.*] config.
    fn resolve_poll_interval_for(&self, provider_name: &str) -> Duration {
        // If there's an explicit per-provider config entry, use the config resolver
        // (which already handles the per-provider override and lifecycle default).
        if self.config.providers.contains_key(provider_name) {
            return self.config.resolve_poll_interval(provider_name);
        }

        // Otherwise, prefer the provider's own metadata interval.
        if let Some(provider) = self.registry.get(provider_name) {
            let meta = provider.metadata();
            let secs = match &meta.invalidation {
                InvalidationStrategy::Poll { interval_secs, .. } => *interval_secs,
                InvalidationStrategy::WatchAndPoll { interval_secs, .. } => *interval_secs,
                InvalidationStrategy::Watch {
                    fallback_poll_secs, ..
                } => fallback_poll_secs.unwrap_or(60),
                // Once providers have no poll cadence; don't fall through to
                // the lifecycle default — that would cause a 60s re-poll and
                // violate the Once contract.
                InvalidationStrategy::Once => return Duration::ZERO,
            };
            if secs > 0 {
                return Duration::from_secs(secs);
            }
        }

        // Final fallback: lifecycle config default.
        self.config.resolve_poll_interval(provider_name)
    }

    fn compute_once_providers(&self) {
        for name in self.registry.list() {
            if let Some(provider) = self.registry.get(&name) {
                let meta = provider.metadata();
                if matches!(meta.invalidation, InvalidationStrategy::Once) {
                    let results = provider.execute(None);
                    if results.is_empty() {
                        warn!(
                            "Provider '{}' returned empty results during initial computation",
                            name
                        );
                    } else {
                        for (scope_path, result) in results {
                            self.cache.put_with_interval(
                                &name,
                                scope_path.as_deref(),
                                result,
                                None,
                            );
                        }
                        info!("Computed initial value for provider '{}'", name);
                    }
                }
            }
        }
    }

    fn handle_fs_event(
        &self,
        paths: Vec<PathBuf>,
        watch_paths: &HashMap<PathBuf, Vec<Subscription>>,
    ) {
        for changed_path in &paths {
            for (watch_path, subscriptions) in watch_paths {
                if !(changed_path.starts_with(watch_path) || changed_path == watch_path) {
                    continue;
                }
                for sub in subscriptions {
                    if event_matches_patterns(&sub.patterns, watch_path, changed_path) {
                        debug!(
                            "FS event: re-executing provider={} path={:?}",
                            sub.provider, sub.path
                        );
                        self.execute_provider(&sub.provider, sub.path.as_deref());
                    }
                }
            }
        }
    }
}

/// Resolve affected lifecycle keys from a set of changed paths using watch_paths + patterns.
fn resolve_keys_from_paths(
    changed_paths: &[PathBuf],
    watch_paths: &HashMap<PathBuf, Vec<Subscription>>,
) -> Vec<(String, Option<String>)> {
    let mut keys = Vec::new();
    for changed_path in changed_paths {
        for (watch_path, subscriptions) in watch_paths {
            if !(changed_path.starts_with(watch_path) || changed_path == watch_path) {
                continue;
            }
            for sub in subscriptions {
                if event_matches_patterns(&sub.patterns, watch_path, changed_path) {
                    keys.push((sub.provider.clone(), sub.path.clone()));
                }
            }
        }
    }
    keys
}

/// Remove watch subscriptions for a specific key and unwatch the path if no subscriptions remain.
fn drop_watches_for_key(
    key: &(String, Option<String>),
    watch_paths: &mut HashMap<PathBuf, Vec<Subscription>>,
    fs_watcher: &mut FsWatcher,
) {
    let mut paths_to_unwatch = Vec::new();
    for (watch_path, subscriptions) in watch_paths.iter_mut() {
        subscriptions.retain(|sub| !(sub.provider == key.0 && sub.path == key.1));
        if subscriptions.is_empty() {
            paths_to_unwatch.push(watch_path.clone());
        }
    }
    for wp in paths_to_unwatch {
        watch_paths.remove(&wp);
        let _ = fs_watcher.unwatch(&wp);
        debug!("Unwatched path {:?} (no subscriptions remaining)", wp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn event_matches_pattern_component_equality() {
        let root = PathBuf::from("/proj");
        assert!(event_matches_patterns(
            &[".git".into()],
            &root,
            Path::new("/proj/.git/HEAD")
        ));
        assert!(event_matches_patterns(
            &["pyproject.toml".into()],
            &root,
            Path::new("/proj/pyproject.toml")
        ));
        assert!(!event_matches_patterns(
            &[".git".into()],
            &root,
            Path::new("/proj/src/main.rs")
        ));
        assert!(!event_matches_patterns(
            &["pyproject.toml".into()],
            &root,
            Path::new("/proj/foo.txt")
        ));
    }

    #[test]
    fn event_matches_patterns_empty_is_fail_open() {
        let root = PathBuf::from("/proj");
        assert!(event_matches_patterns(
            &[],
            &root,
            Path::new("/proj/foo.txt")
        ));
    }

    #[test]
    fn event_matches_patterns_matches_deep_subdirectory() {
        let root = PathBuf::from("/proj");
        assert!(event_matches_patterns(
            &[".git".into()],
            &root,
            Path::new("/proj/packages/foo/.git/COMMIT_EDITMSG"),
        ));
    }

    #[test]
    fn event_matches_patterns_event_on_root_uses_basename() {
        let root = PathBuf::from("/proj/.git");
        assert!(event_matches_patterns(
            &[".git".into()],
            &root,
            Path::new("/proj/.git"),
        ));
    }

    #[test]
    fn event_matches_patterns_event_on_root_no_match() {
        let root = PathBuf::from("/proj");
        assert!(!event_matches_patterns(
            &["pyproject.toml".into()],
            &root,
            Path::new("/proj"),
        ));
    }
}

/// Test helpers for exercising scheduler internals without spinning up the full daemon.
/// Compiled only under `cfg(test)` or when the `test-helpers` Cargo feature is enabled.
/// Release binaries do not include this module.
#[cfg(any(test, feature = "test-helpers"))]
#[doc(hidden)]
pub mod test_support {
    use super::WatcherRegistry;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    /// Spin a lightweight task that fires the GC tick at `gc_interval_dur` using the
    /// real `WatcherRegistry`. Dropping the returned `GcHarness` cancels the task.
    pub fn start_with_gc_interval(
        watchers: Arc<WatcherRegistry>,
        gc_interval_dur: Duration,
    ) -> GcHarness {
        let token = CancellationToken::new();
        let child_token = token.clone();
        let handle = tokio::spawn(async move {
            let mut tick = tokio::time::interval(gc_interval_dur);
            tick.tick().await; // skip the immediate first tick
            loop {
                tokio::select! {
                    _ = child_token.cancelled() => break,
                    _ = tick.tick() => { watchers.gc(); }
                }
            }
        });
        GcHarness {
            token,
            handle: Some(handle),
        }
    }

    pub struct GcHarness {
        token: CancellationToken,
        handle: Option<tokio::task::JoinHandle<()>>,
    }

    impl Drop for GcHarness {
        fn drop(&mut self) {
            self.token.cancel();
            if let Some(h) = self.handle.take() {
                h.abort();
            }
        }
    }
}
