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
use crate::provider::FieldScope;
use crate::provider::InvalidationStrategy;
use crate::provider::registry::ProviderRegistry;
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
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SchedulerStatus {
    pub watched_paths: Vec<String>,
    pub in_flight: Vec<String>,
    pub backoff: Vec<BackoffInfo>,
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
pub struct BackoffInfo {
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
}

// Decay ladder is aspirational — only Grace is wired. SlowPoll / Frozen / Evict
// exist as placeholders for a future rebuild tracked in
// docs/roadmap.md → Known Core Issues. Do not delete variants: removing the
// shape erases the record of what the decay model is supposed to become.
#[derive(Debug, Clone, PartialEq)]
pub enum BackoffStage {
    Grace,
    SlowPoll,
    Frozen,
    Evict,
}

pub struct BackoffState {
    stage: BackoffStage,
    started_at: Instant,
    grace_duration: std::time::Duration,
}

impl BackoffState {
    pub fn new(grace_duration: std::time::Duration) -> Self {
        Self {
            stage: BackoffStage::Grace,
            started_at: Instant::now(),
            grace_duration,
        }
    }

    pub fn stage(&self) -> &BackoffStage {
        &self.stage
    }

    pub fn advance(&mut self) {
        self.stage = match self.stage {
            BackoffStage::Grace => BackoffStage::SlowPoll,
            BackoffStage::SlowPoll => BackoffStage::Frozen,
            BackoffStage::Frozen => BackoffStage::Evict,
            BackoffStage::Evict => BackoffStage::Evict,
        };
        self.started_at = Instant::now();
    }

    pub fn reset(&mut self, grace_duration: std::time::Duration) {
        self.stage = BackoffStage::Grace;
        self.started_at = Instant::now();
        self.grace_duration = grace_duration;
    }

    pub fn elapsed(&self) -> std::time::Duration {
        self.started_at.elapsed()
    }

    pub fn grace_expired(&self) -> bool {
        matches!(self.stage, BackoffStage::Grace)
            && self.started_at.elapsed() >= self.grace_duration
    }

    // Never called in production — placeholder for decay ladder (see BackoffStage comment).
    pub fn should_watch(&self) -> bool {
        matches!(self.stage, BackoffStage::Grace)
    }
}

/// Tracks the last execution time for polling purposes.
struct PollState {
    last_run: Instant,
    interval_secs: u64,
}

/// Advance backoff states. Returns the list of keys that should be evicted from cache.
fn check_backoff(
    backoff: &mut HashMap<(String, Option<String>), BackoffState>,
) -> Vec<(String, Option<String>)> {
    let mut to_evict = Vec::new();
    let mut to_advance = Vec::new();

    for (key, state) in backoff.iter() {
        match state.stage() {
            BackoffStage::Grace if state.grace_expired() => {
                to_advance.push(key.clone());
            }
            BackoffStage::Evict => {
                to_evict.push(key.clone());
            }
            _ => {}
        }
    }

    for key in &to_advance {
        if let Some(state) = backoff.get_mut(key) {
            debug!("Advancing backoff for provider={} path={:?}", key.0, key.1);
            state.advance();
        }
    }

    // Remove evicted keys from backoff tracking.
    for key in &to_evict {
        debug!(
            "Removing backoff entry (evict) for provider={} path={:?}",
            key.0, key.1
        );
        backoff.remove(key);
    }

    to_evict
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

/// Build a snapshot of the current scheduler state for status reporting.
fn build_status(
    poll_states: &HashMap<(String, Option<String>), PollState>,
    watch_paths: &HashMap<PathBuf, Vec<Subscription>>,
    backoff: &HashMap<(String, Option<String>), BackoffState>,
    in_flight: &std::sync::Mutex<std::collections::HashSet<(String, Option<String>)>>,
    demand: &HashMap<(String, Option<String>), Instant>,
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

    let backoff_info: Vec<BackoffInfo> = backoff
        .iter()
        .map(|((provider, path), state)| BackoffInfo {
            provider: provider.clone(),
            path: path.clone(),
            stage: format!("{:?}", state.stage()),
            elapsed_secs: state.elapsed().as_secs(),
        })
        .collect();

    let poll_timer_info: Vec<PollTimerInfo> = poll_states
        .iter()
        .map(|((provider, path), state)| PollTimerInfo {
            provider: provider.clone(),
            path: path.clone(),
            interval_secs: state.interval_secs,
            last_run_secs_ago: state.last_run.elapsed().as_secs(),
        })
        .collect();

    let demand_info: Vec<DemandInfo> = demand
        .iter()
        .map(|((provider, path), last_query)| DemandInfo {
            provider: provider.clone(),
            path: path.clone(),
            last_query_secs_ago: last_query.elapsed().as_secs(),
        })
        .collect();

    SchedulerStatus {
        watched_paths: watched,
        in_flight: in_flight_keys,
        backoff: backoff_info,
        poll_timers: poll_timer_info,
        demand: demand_info,
    }
}

/// Start backoff for a single key if not already in backoff.
fn start_backoff_for_key(
    key: &(String, Option<String>),
    backoff: &mut HashMap<(String, Option<String>), BackoffState>,
    config: &Config,
) {
    let grace_duration =
        config.resolve_poll_interval(&key.0) * config.resolve_poll_live_count(&key.0);
    backoff.entry(key.clone()).or_insert_with(|| {
        debug!("Starting backoff for provider={} path={:?}", key.0, key.1);
        BackoffState::new(grace_duration)
    });
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

        // Poll states: (provider, path) -> PollState
        let mut poll_states: HashMap<(String, Option<String>), PollState> = HashMap::new();

        // Backoff states for keys with no active subscribers.
        let mut backoff: HashMap<(String, Option<String>), BackoffState> = HashMap::new();

        // Watch paths that are being monitored: path -> subscriptions
        let mut watch_paths: HashMap<PathBuf, Vec<Subscription>> = HashMap::new();

        // Demand tracking: (provider, path) -> last query time.
        // Keys queried within the demand window are kept warm with default polling.
        let mut demand: HashMap<(String, Option<String>), Instant> = HashMap::new();
        let demand_window_secs: u64 = 120; // Keep warm for 2 minutes after last query

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
                            let status = build_status(&poll_states, &watch_paths, &backoff, &self.in_flight, &demand);
                            let _ = reply.send(status);
                        }
                        Some(SchedulerMessage::QueryActivity { provider, path }) => {
                            let key = (provider.clone(), path.clone());
                            let is_new = !demand.contains_key(&key);
                            demand.insert(key.clone(), Instant::now());
                            last_activity = Instant::now();

                            // Cancel backoff if this key was draining.
                            if backoff.remove(&key).is_some() {
                                debug!("Cancelled backoff for provider={} path={:?} (query demand)", provider, path);
                            }

                            // Restore live polling interval if we were in idle mode.
                            if let Some(prov) = self.registry.get(&provider) {
                                let meta = prov.metadata();
                                let meta_poll_secs = match &meta.invalidation {
                                    InvalidationStrategy::Poll { interval_secs, .. } => *interval_secs,
                                    InvalidationStrategy::WatchAndPoll { interval_secs, .. } => *interval_secs,
                                    InvalidationStrategy::Watch { fallback_poll_secs, .. } => fallback_poll_secs.unwrap_or(60),
                                    InvalidationStrategy::Once => 0,
                                };
                                let live_poll_secs = if self.config.providers.contains_key(&provider) {
                                    self.config.resolve_poll_interval(&provider).as_secs()
                                } else {
                                    meta_poll_secs
                                };
                                if let Some(state) = poll_states.get_mut(&key)
                                    && state.interval_secs != live_poll_secs
                                    && live_poll_secs > 0
                                {
                                    debug!("Restored live polling for provider={} path={:?} every {}s", provider, path, live_poll_secs);
                                    state.interval_secs = live_poll_secs;
                                }
                            }

                            // If this is new demand, set up default polling based on provider metadata.
                            if is_new {
                                if let Some(prov) = self.registry.get(&provider) {
                                    let meta = prov.metadata();
                                    let poll_secs = match &meta.invalidation {
                                        InvalidationStrategy::Poll { interval_secs, .. } => *interval_secs,
                                        InvalidationStrategy::WatchAndPoll { interval_secs, .. } => *interval_secs,
                                        InvalidationStrategy::Watch { fallback_poll_secs, .. } => fallback_poll_secs.unwrap_or(60),
                                        InvalidationStrategy::Once => 0, // No polling for Once providers
                                    };

                                    // Apply config override if set
                                    let poll_secs = if self.config.providers.contains_key(&provider) {
                                        self.config.resolve_poll_interval(&provider).as_secs()
                                    } else {
                                        poll_secs
                                    };

                                    if poll_secs > 0 {
                                        poll_states.entry(key.clone()).or_insert(PollState {
                                            last_run: Instant::now(),
                                            interval_secs: poll_secs,
                                        });
                                        debug!("Demand: started polling provider={} path={:?} every {}s", provider, path, poll_secs);
                                    }

                                    // Set up filesystem watching for path-scoped providers.
                                    if meta.inferred_scope() == FieldScope::PathScoped
                                        && let Some(ref path_str) = path {
                                        let watch_path = PathBuf::from(path_str);
                                        if let Err(e) = fs_watcher.watch(&watch_path) {
                                            warn!("Failed to watch {:?}: {}", watch_path, e);
                                        } else {
                                            let patterns = crate::provider::watch_patterns(
                                                &meta.invalidation,
                                            );
                                            watch_paths
                                                .entry(watch_path)
                                                .or_default()
                                                .push(Subscription {
                                                    provider: provider.clone(),
                                                    path: path.clone(),
                                                    patterns,
                                                });
                                            debug!("Demand: watching path {:?} for provider={}", path, provider);
                                        }
                                    }
                                }

                                // Execute immediately if not cached.
                                if self.cache.get(&provider, path.as_deref()).is_none() {
                                    self.execute_provider(&provider, path.as_deref());
                                }
                            }
                        }
                    }
                }

                // Filesystem events from watcher.
                Some(paths) = fs_rx.recv() => {
                    self.handle_fs_event(paths, &watch_paths);
                }

                // Periodic GC: remove dead broadcast channel entries.
                _ = gc_tick.tick() => {
                    self.watchers.gc();
                }

                // Poll tick — check which subscriptions are due.
                _ = tick.tick() => {
                    self.heartbeat.fetch_add(1, Ordering::Relaxed);

                    let now = Instant::now();
                    let keys_to_run: Vec<(String, Option<String>)> = poll_states
                        .iter()
                        .filter(|(_, state)| {
                            now.duration_since(state.last_run).as_secs() >= state.interval_secs
                        })
                        .map(|(key, _)| key.clone())
                        .collect();

                    for (provider, path) in keys_to_run {
                        debug!("Poll tick: executing provider={} path={:?}", provider, path);
                        self.execute_provider(&provider, path.as_deref());
                        if let Some(state) = poll_states.get_mut(&(provider, path)) {
                            state.last_run = Instant::now();
                        }
                    }

                    // Check demand expiry — stop polling/watching keys nobody has queried recently.
                    let now = Instant::now();
                    let expired_demand: Vec<(String, Option<String>)> = demand
                        .iter()
                        .filter(|(_, last_query)| now.duration_since(**last_query).as_secs() >= demand_window_secs)
                        .map(|(key, _)| key.clone())
                        .collect();

                    for key in expired_demand {
                        debug!("Demand expired for provider={} path={:?}", key.0, key.1);
                        demand.remove(&key);

                        // poll_idle_interval removed; stop polling on demand expiry.
                        poll_states.remove(&key);

                        // Remove watch for this key (watches are only active during live demand).
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
                        }

                        // Start backoff for eviction.
                        start_backoff_for_key(&key, &mut backoff, &self.config);
                    }

                    // Advance backoff states and evict cache entries when needed.
                    let keys_to_evict = check_backoff(&mut backoff);
                    for (provider, path) in keys_to_evict {
                        debug!("Evicting cache for provider={} path={:?} (backoff evict)", provider, path);
                        self.cache.remove(&provider, path.as_deref());
                    }

                    // Check idle shutdown condition.
                    if let Some(idle_secs) = idle_shutdown_secs
                        && self.cache.is_empty()
                        && demand.is_empty()
                        && last_activity.elapsed().as_secs() >= idle_secs
                    {
                        info!("Idle shutdown: no cache entries, no demand, idle for {}s", idle_secs);
                        break;
                    }
                }
            }
        }
    }

    /// Fallback run loop when FsWatcher creation fails — no watch support.
    async fn run_without_watcher(mut self, mut _dummy_rx: mpsc::Receiver<Vec<PathBuf>>) {
        self.compute_once_providers();

        let mut poll_states: HashMap<(String, Option<String>), PollState> = HashMap::new();
        let mut backoff: HashMap<(String, Option<String>), BackoffState> = HashMap::new();

        // Demand tracking: (provider, path) -> last query time.
        let mut demand: HashMap<(String, Option<String>), Instant> = HashMap::new();
        let demand_window_secs: u64 = 120;

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
                            let status = build_status(&poll_states, &empty_watch_paths, &backoff, &self.in_flight, &demand);
                            let _ = reply.send(status);
                        }
                        Some(SchedulerMessage::QueryActivity { provider, path }) => {
                            let key = (provider.clone(), path.clone());
                            let is_new = !demand.contains_key(&key);
                            demand.insert(key.clone(), Instant::now());
                            last_activity = Instant::now();

                            // Cancel backoff if this key was draining.
                            if backoff.remove(&key).is_some() {
                                debug!("Cancelled backoff for provider={} path={:?} (query demand)", provider, path);
                            }

                            // Restore live polling interval if we were in idle mode.
                            if let Some(prov) = self.registry.get(&provider) {
                                let meta = prov.metadata();
                                let meta_poll_secs = match &meta.invalidation {
                                    InvalidationStrategy::Poll { interval_secs, .. } => *interval_secs,
                                    InvalidationStrategy::WatchAndPoll { interval_secs, .. } => *interval_secs,
                                    InvalidationStrategy::Watch { fallback_poll_secs, .. } => fallback_poll_secs.unwrap_or(60),
                                    InvalidationStrategy::Once => 0,
                                };
                                let live_poll_secs = if self.config.providers.contains_key(&provider) {
                                    self.config.resolve_poll_interval(&provider).as_secs()
                                } else {
                                    meta_poll_secs
                                };
                                if let Some(state) = poll_states.get_mut(&key)
                                    && state.interval_secs != live_poll_secs
                                    && live_poll_secs > 0
                                {
                                    debug!("Restored live polling for provider={} path={:?} every {}s", provider, path, live_poll_secs);
                                    state.interval_secs = live_poll_secs;
                                }
                            }

                            // If this is new demand, set up default polling based on provider metadata.
                            if is_new {
                                if let Some(prov) = self.registry.get(&provider) {
                                    let meta = prov.metadata();
                                    let poll_secs = match &meta.invalidation {
                                        InvalidationStrategy::Poll { interval_secs, .. } => *interval_secs,
                                        InvalidationStrategy::WatchAndPoll { interval_secs, .. } => *interval_secs,
                                        InvalidationStrategy::Watch { fallback_poll_secs, .. } => fallback_poll_secs.unwrap_or(60),
                                        InvalidationStrategy::Once => 0,
                                    };

                                    // Apply config override if set
                                    let poll_secs = if self.config.providers.contains_key(&provider) {
                                        self.config.resolve_poll_interval(&provider).as_secs()
                                    } else {
                                        poll_secs
                                    };

                                    if poll_secs > 0 {
                                        poll_states.entry(key.clone()).or_insert(PollState {
                                            last_run: Instant::now(),
                                            interval_secs: poll_secs,
                                        });
                                        debug!("Demand: started polling provider={} path={:?} every {}s", provider, path, poll_secs);
                                    }
                                }

                                // Execute immediately if not cached.
                                if self.cache.get(&provider, path.as_deref()).is_none() {
                                    self.execute_provider(&provider, path.as_deref());
                                }
                            }
                        }
                    }
                }
                // Periodic GC: remove dead broadcast channel entries.
                _ = gc_tick.tick() => {
                    self.watchers.gc();
                }
                _ = tick.tick() => {
                    self.heartbeat.fetch_add(1, Ordering::Relaxed);

                    let now = Instant::now();
                    let keys_to_run: Vec<(String, Option<String>)> = poll_states
                        .iter()
                        .filter(|(_, state)| {
                            now.duration_since(state.last_run).as_secs() >= state.interval_secs
                        })
                        .map(|(key, _)| key.clone())
                        .collect();

                    for (provider, path) in keys_to_run {
                        self.execute_provider(&provider, path.as_deref());
                        if let Some(state) = poll_states.get_mut(&(provider, path)) {
                            state.last_run = Instant::now();
                        }
                    }

                    // Check demand expiry — stop polling keys nobody has queried recently.
                    let now = Instant::now();
                    let expired_demand: Vec<(String, Option<String>)> = demand
                        .iter()
                        .filter(|(_, last_query)| now.duration_since(**last_query).as_secs() >= demand_window_secs)
                        .map(|(key, _)| key.clone())
                        .collect();

                    for key in expired_demand {
                        debug!("Demand expired for provider={} path={:?}", key.0, key.1);
                        demand.remove(&key);

                        // poll_idle_interval removed; stop polling on demand expiry.
                        poll_states.remove(&key);

                        // Start backoff for eviction.
                        start_backoff_for_key(&key, &mut backoff, &self.config);
                    }

                    // Advance backoff states and evict cache entries when needed.
                    let keys_to_evict = check_backoff(&mut backoff);
                    for (provider, path) in keys_to_evict {
                        debug!("Evicting cache for provider={} path={:?} (backoff evict)", provider, path);
                        self.cache.remove(&provider, path.as_deref());
                    }

                    // Check idle shutdown condition.
                    if let Some(idle_secs) = idle_shutdown_secs
                        && self.cache.is_empty()
                        && demand.is_empty()
                        && last_activity.elapsed().as_secs() >= idle_secs
                    {
                        info!("Idle shutdown: no cache entries, no demand, idle for {}s", idle_secs);
                        break;
                    }
                }
            }
        }
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
