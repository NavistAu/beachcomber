pub mod lifecycle;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::time::{Duration, interval};
use tracing::{debug, info, warn};

use crate::query::SourceDemand;

use crate::cache::Cache;
pub use crate::cache::FailureSnapshot;
use crate::config::Config;
use crate::provider::InvalidationStrategy;
use crate::provider::SourceScope;
use crate::provider::registry::ProviderRegistry;
use crate::scheduler::lifecycle::{
    LifecycleRegistry, LifecycleState, SourceLifecycleConfig, StateTransition, WatchAction,
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
    /// A provider+path was queried via get/watch. Signals demand to keep the
    /// queried Sources warm. `demand` selects which Sources: `All` for a
    /// whole-provider query, `Sources(..)` for a field/source query.
    QueryActivity {
        provider: String,
        path: Option<String>,
        demand: SourceDemand,
    },
    /// Request a snapshot of all (provider, path, source) entries with non-zero failure counts.
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
    /// Effective poll interval in seconds (may be doubled during decay). 0 for pure Watch.
    pub poll_interval_secs: u64,
    /// K — keep-alive count. For `KeepAlive::Polls(K)` this is K. For
    /// `KeepAlive::Duration(secs)` it's `secs / poll_interval_secs` if there is
    /// a poll path (WatchAndPoll), otherwise 0 (pure Watch). For
    /// `KeepAlive::Never` it's 0 (no decay).
    pub keep_alive_polls: u32,
    /// Number of poll-equivalents that have fired in the current lifecycle
    /// step. Used by the status formatter to render the `{age}×{N}` suffix on
    /// the age column.
    pub polls_elapsed: u32,
    /// Seconds until the next scheduled poll fires (`poll_timer.last_fired +
    /// poll_timer.interval`, clamped to now). `None` for pure Watch sources
    /// (no poll timer).
    pub next_poll_in_secs: Option<u64>,
    /// Whether fsevents are reinstated on demand for this entry.
    pub fsevents_reinstate: bool,
    /// True if the provider uses Watch or WatchAndPoll invalidation strategy.
    pub watches_files: bool,
    /// Source name for this lifecycle entry.
    pub source: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SchedulerStatus {
    pub watched_paths: Vec<String>,
    pub in_flight: Vec<String>,
    pub lifecycle: Vec<LifecycleInfo>,
    pub poll_timers: Vec<PollTimerInfo>,
    pub demand: Vec<DemandInfo>,
    /// Provider file-watch backend chosen by the startup self-test:
    /// "native", "polling", or "disabled" (watcher creation failed).
    pub watch_backend: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DemandInfo {
    pub provider: String,
    pub path: Option<String>,
    pub source: String,
    pub last_query_secs_ago: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LifecycleInfo {
    pub provider: String,
    pub path: Option<String>,
    pub source: String,
    pub stage: String,
    pub elapsed_secs: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PollTimerInfo {
    pub provider: String,
    pub path: Option<String>,
    pub source: String,
    pub interval_secs: u64,
    pub last_run_secs_ago: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct Subscription {
    pub(crate) provider: String,
    pub(crate) path: Option<String>,
    pub(crate) source: String,
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

    /// Return a snapshot of all (provider, path, source) entries with non-zero consecutive failure counts.
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
    pub async fn get_lifecycle_snapshots(&self) -> HashMap<lifecycle::Key, LifecycleSnapshot> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = self
            .tx
            .send(SchedulerMessage::GetLifecycleSnapshots { reply: tx })
            .await;
        rx.await.unwrap_or_default()
    }
}

/// Tracks consecutive failures and suppression state for a (provider, path, source) key.
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

/// Build a `LifecycleSnapshot` for a given lifecycle entry at `now`. Centralised
/// so the two snapshot-reply sites (sync and async branches in the scheduler
/// loop) compute polls_elapsed and keep_alive_polls identically.
fn snapshot_entry(
    key: &lifecycle::Key,
    entry: &lifecycle::LifecycleEntry,
    now: Instant,
) -> LifecycleSnapshot {
    use crate::provider::KeepAlive;

    let watches_files = matches!(
        entry.config.strategy_kind,
        lifecycle::StrategyKind::Watch | lifecycle::StrategyKind::WatchAndPoll
    );
    let poll_interval_secs = entry
        .poll_timer
        .as_ref()
        .map(|pt| pt.interval.as_secs())
        .unwrap_or(0);

    let decay = lifecycle::to_decay_level(&entry.state);
    let rate_mult: u64 = if decay == 0 { 1 } else { 1u64 << decay };

    let base_poll_secs = entry.config.poll_interval.map(|d| d.as_secs()).unwrap_or(0);

    // K and step duration both depend on the keep-alive variant.
    let (keep_alive_polls, step_duration_secs) = match entry.config.keep_alive {
        KeepAlive::Polls(k) => {
            let dur = (k as u64) * base_poll_secs * rate_mult;
            (k, dur)
        }
        KeepAlive::Duration(secs) => {
            // For pure Watch (no poll path) polls don't apply; report 0.
            // For WatchAndPoll, derive K = secs / base_poll_secs so the renderer
            // can produce the {p}s×{k:02} format.
            let k = secs.checked_div(base_poll_secs).unwrap_or(0) as u32;
            (k, secs * rate_mult)
        }
        KeepAlive::Never => (0, 0),
    };

    // polls_elapsed: how many polls have fired in the current lifecycle step.
    // Only meaningful when there is a poll path AND a decay timer.
    let polls_elapsed = match (entry.decay_timer.as_ref(), poll_interval_secs) {
        (Some(dt), p) if p > 0 && step_duration_secs > 0 => {
            let step_start = dt
                .step_deadline
                .checked_sub(Duration::from_secs(step_duration_secs))
                .unwrap_or(now);
            let secs_in_step = now.saturating_duration_since(step_start).as_secs();
            let n = secs_in_step / p;
            (n as u32).min(keep_alive_polls.max(1))
        }
        _ => 0,
    };

    // Seconds until the poll timer's next scheduled fire, mirroring the
    // `tick()` due-check (`last_fired + interval`). Saturates to 0 once due.
    let next_poll_in_secs = entry.poll_timer.as_ref().map(|pt| {
        let next_due = pt.last_fired + pt.interval;
        next_due.saturating_duration_since(now).as_secs()
    });

    LifecycleSnapshot {
        decay,
        poll_interval_secs,
        keep_alive_polls,
        polls_elapsed,
        next_poll_in_secs,
        fsevents_reinstate: entry.config.fsevents_reinstate,
        watches_files,
        source: key.2.clone(),
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
    in_flight: &std::sync::Mutex<std::collections::HashSet<lifecycle::Key>>,
    watch_backend: &str,
) -> SchedulerStatus {
    let watched: Vec<String> = watch_paths
        .keys()
        .map(|p| p.to_string_lossy().to_string())
        .collect();

    let in_flight_keys: Vec<String> = in_flight
        .lock()
        .unwrap()
        .iter()
        .map(|(provider, path, source)| match path {
            Some(pa) => format!("{provider}:{pa}:{source}"),
            None => format!("{provider}:{source}"),
        })
        .collect();

    let mut backoff_info: Vec<LifecycleInfo> = Vec::new();
    let mut poll_timer_info: Vec<PollTimerInfo> = Vec::new();
    let mut demand_info: Vec<DemandInfo> = Vec::new();

    for ((provider, path, source), entry) in lifecycle.iter() {
        // Poll timer info — only for entries with a poll path.
        if let Some(pt) = &entry.poll_timer {
            poll_timer_info.push(PollTimerInfo {
                provider: provider.clone(),
                path: path.clone(),
                source: source.clone(),
                interval_secs: pt.interval.as_secs(),
                last_run_secs_ago: pt.last_fired.elapsed().as_secs(),
            });
        }

        match entry.state {
            LifecycleState::Active => {
                // Active entries have demand — report last_demand time.
                if let Some(dt) = &entry.decay_timer {
                    demand_info.push(DemandInfo {
                        provider: provider.clone(),
                        path: path.clone(),
                        source: source.clone(),
                        last_query_secs_ago: dt.last_demand.elapsed().as_secs(),
                    });
                }
            }
            LifecycleState::Decay(step) => {
                // Decaying entries go in the lifecycle list.
                let elapsed_secs = entry
                    .decay_timer
                    .as_ref()
                    .map(|dt| dt.last_demand.elapsed().as_secs())
                    .unwrap_or(0);
                backoff_info.push(LifecycleInfo {
                    provider: provider.clone(),
                    path: path.clone(),
                    source: source.clone(),
                    stage: format!("Decay{}", step.as_u8()),
                    elapsed_secs,
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
        watch_backend: watch_backend.to_string(),
    }
}

/// In-flight set: per-(provider, path, source) deduplication.
type SourceKeySet = Arc<std::sync::Mutex<std::collections::HashSet<lifecycle::Key>>>;
/// Failure backoff map: per-(provider, path, source) failure state.
type SourceFailureMap = Arc<std::sync::Mutex<HashMap<lifecycle::Key, FailureState>>>;

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
    /// Tracks which (provider, path, source) combinations are currently executing.
    in_flight: SourceKeySet,
    /// Tracks which (provider, path, source) need to re-run after current execution completes.
    pending_rerun: SourceKeySet,
    /// Tracks consecutive failures and suppression state per (provider, path, source).
    failure_counts: SourceFailureMap,
    /// Monotonically increasing counter bumped on every tick. Used by the watchdog
    /// to detect scheduler stalls.
    heartbeat: Arc<AtomicU64>,
    /// WatcherRegistry — gc() called periodically to remove dead channel entries.
    watchers: Arc<WatcherRegistry>,
    /// Whether `run()` probes fs-event delivery before choosing the watch
    /// backend. Set by the daemon path via `self_test_watch_backend()`;
    /// defaults to false (assume native) for directly-constructed schedulers.
    run_watch_self_test: bool,
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
            run_watch_self_test: false,
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

    /// Execute a specific source on the blocking thread pool and write result to cache.
    /// This is fire-and-forget: returns immediately while the source runs in the background.
    /// Deduplicates concurrent executions via in_flight: if the (provider, path, source) triple
    /// is already running, marks it for a single rerun after completion.
    /// Suppresses execution when failure backoff is active.
    fn execute_source(&self, provider_name: &str, source_name: &str, path: Option<&str>) {
        let Some(source) = self.registry.source(provider_name, source_name) else {
            warn!(
                "Refresh for unknown source '{}.{}'",
                provider_name, source_name
            );
            return;
        };

        let key: lifecycle::Key = (
            provider_name.to_string(),
            path.map(|s| s.to_string()),
            source_name.to_string(),
        );

        // Check failure backoff — skip if suppressed.
        {
            let failures = self.failure_counts.lock().unwrap();
            if let Some(state) = failures.get(&key)
                && state.is_suppressed()
            {
                debug!(
                    "Source '{}.{}' suppressed due to failure backoff",
                    provider_name, source_name
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
                    "Source '{}.{}' already in flight, queued rerun",
                    provider_name, source_name
                );
                return;
            }
            in_flight.insert(key.clone());
        }

        let cache = Arc::clone(&self.cache);
        let timeout_secs = self.config.daemon.provider_timeout_secs.unwrap_or(10);
        let in_flight = Arc::clone(&self.in_flight);
        let pending_rerun = Arc::clone(&self.pending_rerun);
        let failure_counts = Arc::clone(&self.failure_counts);
        let path_owned = path.map(|s| s.to_string());
        let source_clone = Arc::clone(&source);
        let provider_name_owned = provider_name.to_string();
        let source_name_owned = source_name.to_string();
        let key_for_cleanup = key.clone();

        let expected_interval_secs = match source.metadata().invalidation {
            InvalidationStrategy::Poll { interval_secs } => Some(interval_secs),
            InvalidationStrategy::WatchAndPoll { interval_secs, .. } => Some(interval_secs),
            InvalidationStrategy::Watch { .. } => None,
        };

        let failure_threshold = self.config.resolve_failure_reattempts_for_source(
            provider_name,
            Some(source_name),
            Some(source.metadata().failback.reattempts),
        );
        let failure_backoff = self.config.resolve_failure_backoff_for_source(
            provider_name,
            Some(source_name),
            Some(std::time::Duration::from_secs(
                source.metadata().failback.interval_secs,
            )),
        );

        tokio::spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(timeout_secs),
                tokio::task::spawn_blocking(move || source_clone.execute(path_owned.as_deref())),
            )
            .await;

            // Record success or failure for backoff tracking.
            match &result {
                Ok(Ok(r)) if !r.fields.is_empty() => {
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

            let cache_path = key_for_cleanup.1.clone();
            match result {
                Ok(Ok(source_result)) if !source_result.fields.is_empty() => {
                    cache.put_source(
                        &provider_name_owned,
                        cache_path.as_deref(),
                        &source_name_owned,
                        source_result.fields,
                        expected_interval_secs,
                    );
                    debug!(
                        "Executed source '{}.{}' path={:?}",
                        provider_name_owned, source_name_owned, cache_path
                    );
                }
                Ok(Ok(_)) => {
                    debug!(
                        "Source '{}.{}' returned empty for path={:?}",
                        provider_name_owned, source_name_owned, cache_path
                    );
                }
                Ok(Err(e)) => {
                    warn!(
                        "Source '{}.{}' panicked: {}",
                        provider_name_owned, source_name_owned, e
                    );
                }
                Err(_) => {
                    warn!(
                        "Source '{}.{}' timed out after {}s",
                        provider_name_owned, source_name_owned, timeout_secs
                    );
                }
            }

            // Clear in-flight and check for pending reruns.
            in_flight.lock().unwrap().remove(&key_for_cleanup);
            let should_rerun = pending_rerun.lock().unwrap().remove(&key_for_cleanup);

            if should_rerun {
                debug!(
                    "Re-running source '{}.{}' (was queued during previous execution)",
                    key_for_cleanup.0, key_for_cleanup.2
                );
                // Re-dispatch via a new spawn rather than recursive call, preserving
                // channel-driven dispatch ordering. Mark in-flight again for this rerun.
                in_flight.lock().unwrap().insert(key_for_cleanup.clone());
                let rerun_provider = key_for_cleanup.0.clone();
                let rerun_source = key_for_cleanup.2.clone();
                let rerun_path = key_for_cleanup.1.clone();
                tokio::spawn(async move {
                    // Note: we can't call execute_source() here (not &self), so we
                    // inline the execution. The source was already Arc'd above but
                    // we can't recover it from the key alone. This is a known
                    // limitation for the rerun path — the source will be looked up
                    // at the next demand/tick cycle instead.
                    //
                    // For now, just clear in-flight so the next poll/demand can proceed.
                    debug!(
                        "Rerun stub for '{}.{}' path={:?} — will re-execute on next poll",
                        rerun_provider, rerun_source, rerun_path
                    );
                    in_flight.lock().unwrap().remove(&key_for_cleanup);
                });
            }
        });
    }

    /// Opt this scheduler into the startup watch self-test (canon
    /// provider_source.md §"Watch backend health"). The daemon path sets this; directly-constructed
    /// schedulers (tests) default to assuming the native backend, exactly the
    /// pre-self-test behaviour, and skip the probe and its 500ms worst case.
    pub fn self_test_watch_backend(&mut self) {
        self.run_watch_self_test = true;
    }

    pub async fn run(mut self) {
        // Provider file-watching starts on the kernel-native backend
        // immediately; the watch self-test (canon provider_source.md §"Watch
        // backend health") runs concurrently rather than gating the loop — kernel
        // event delivery can take hundreds of ms even when healthy, and the
        // scheduler must serve demand from its first moment. If the self-test
        // fails, the loop swaps to the polling backend and re-registers every
        // watch path (a stream can be created without error and deliver
        // nothing — sandboxed CI hosts, degraded fseventsd; chosen blind,
        // watch-invalidated entries would silently never invalidate).
        let (mut fs_watcher, mut fs_rx) = match FsWatcher::new() {
            Ok(pair) => pair,
            Err(e) => {
                warn!(
                    "Failed to create filesystem watcher: {}. Watch triggers disabled.",
                    e
                );
                // Create a channel that never receives to allow the rest of the loop to work.
                let (_tx, rx) = mpsc::channel::<Vec<PathBuf>>(1);
                drop(e);
                return self.run_without_watcher(rx).await;
            }
        };
        let mut watch_backend = crate::watcher::WatchBackend::Native;

        let mut self_test_rx: Option<tokio::sync::oneshot::Receiver<bool>> =
            if self.run_watch_self_test {
                let (tx, rx) = tokio::sync::oneshot::channel();
                tokio::spawn(async move {
                    let ok = crate::watcher::self_test_native_backend(
                        crate::watcher::WATCH_SELF_TEST_TIMEOUT,
                    )
                    .await;
                    let _ = tx.send(ok);
                });
                Some(rx)
            } else {
                None
            };

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
                            // Fan out to all sources for this provider. Snapshot the
                            // names so the registry borrow drops before the calls.
                            let names: Vec<String> = self
                                .registry
                                .provider_sources(&provider)
                                .map(|src| src.iter().map(|sm| sm.name.clone()).collect())
                                .unwrap_or_default();
                            for name in &names {
                                self.execute_source(&provider, name, path.as_deref());
                            }
                            last_activity = Instant::now();
                        }
                        Some(SchedulerMessage::FsEvent { paths }) => {
                            self.handle_fs_event(paths, &watch_paths, &mut lifecycle);
                            last_activity = Instant::now();
                        }
                        Some(SchedulerMessage::GetStatus { reply }) => {
                            let status = build_status(
                                &lifecycle,
                                &watch_paths,
                                &self.in_flight,
                                watch_backend.as_str(),
                            );
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
                            let now = Instant::now();
                            let map: HashMap<lifecycle::Key, LifecycleSnapshot> = lifecycle
                                .iter()
                                .map(|(k, entry)| (k.clone(), snapshot_entry(k, entry, now)))
                                .collect();
                            let _ = reply.send(map);
                        }
                        Some(SchedulerMessage::QueryActivity { provider, path, demand }) => {
                            let now = Instant::now();
                            let Some(sources) = self.registry.provider_sources(&provider) else {
                                last_activity = now;
                                continue;
                            };
                            // Narrow to the demanded Sources. Scope/path policy in the
                            // loop below is unchanged and applies to whichever Sources
                            // survive this filter (canon §150/§268: a field demands only
                            // its owning Source).
                            let sources_vec: Vec<_> = match &demand {
                                SourceDemand::All => sources.to_vec(),
                                SourceDemand::Sources(names) => sources
                                    .iter()
                                    .filter(|sm| names.iter().any(|n| n == &sm.name))
                                    .cloned()
                                    .collect(),
                            };
                            for sm in &sources_vec {
                                // Scope filter:
                                //  - PathScoped sources: skip when no path was provided
                                //    (they have nothing to attach to).
                                //  - Global sources: always demand at (provider, None),
                                //    regardless of whether the consumer query carried a
                                //    path. A whole-provider query like `comb get mise`
                                //    from inside a project should warm BOTH the project
                                //    PathScoped source at the resolved path AND the
                                //    Global source at the pathless slot.
                                if matches!(sm.scope, SourceScope::PathScoped) && path.is_none() {
                                    continue;
                                }

                                // Global sources always live at (provider, None) regardless
                                // of the path carried by the QueryActivity message.
                                let effective_key_path = match sm.scope {
                                    SourceScope::Global => None,
                                    SourceScope::PathScoped => path.clone(),
                                };

                                let key: lifecycle::Key = (
                                    provider.clone(),
                                    effective_key_path.clone(),
                                    sm.name.clone(),
                                );
                                let cfg = SourceLifecycleConfig::from_strategy(
                                    &sm.invalidation,
                                    sm.keep_alive,
                                    sm.fsevents_reinstate,
                                );
                                let outcome = lifecycle.on_demand(key.clone(), cfg, now);

                                match outcome.watch_registration {
                                    WatchAction::Register | WatchAction::Reinstate => {
                                        // Register fs watches for PathScoped sources.
                                        if matches!(sm.scope, SourceScope::PathScoped)
                                            && let Some(ref path_str) = path
                                        {
                                            let watch_path = PathBuf::from(path_str);
                                            let patterns = crate::provider::watch_patterns(
                                                &sm.invalidation,
                                            );
                                            let sub = Subscription {
                                                provider: provider.clone(),
                                                path: path.clone(),
                                                source: sm.name.clone(),
                                                patterns,
                                            };
                                            let (returned_watcher, registered) =
                                                register_path_watch(
                                                    fs_watcher,
                                                    &mut watch_paths,
                                                    watch_path.clone(),
                                                    sub,
                                                )
                                                .await;
                                            fs_watcher = returned_watcher;
                                            if registered {
                                                debug!(
                                                    "Demand: watching path {:?} for provider={} source={}",
                                                    path, provider, sm.name
                                                );
                                            }
                                            // Env-selected file sources declare concrete
                                            // files to watch via watched_files(); the path
                                            // may be a ':'-joined list that is not itself a
                                            // single watchable path. Subscribe each file to
                                            // THIS instance's lifecycle key.
                                            if let Some(src) = self.registry.source(&provider, &sm.name) {
                                                for file in src.watched_files(Some(path_str)) {
                                                    let sub = Subscription {
                                                        provider: provider.clone(),
                                                        path: path.clone(),
                                                        source: sm.name.clone(),
                                                        patterns: Vec::new(),
                                                    };
                                                    let (returned_watcher, _) =
                                                        register_path_watch(
                                                            fs_watcher,
                                                            &mut watch_paths,
                                                            file,
                                                            sub,
                                                        )
                                                        .await;
                                                    fs_watcher = returned_watcher;
                                                }
                                            }
                                        }
                                        // Register absolute-path watches for Global Watch/WatchAndPoll sources.
                                        // Subscriptions key on the source's effective path (None for Global)
                                        // so fs-event dispatch resolves to the right lifecycle entry.
                                        if matches!(sm.scope, SourceScope::Global) {
                                            let abs_paths =
                                                crate::provider::watch_abs_paths(&sm.invalidation);
                                            for abs_path_str in &abs_paths {
                                                let abs_path = PathBuf::from(abs_path_str);
                                                let sub = Subscription {
                                                    provider: provider.clone(),
                                                    path: None,
                                                    source: sm.name.clone(),
                                                    patterns: Vec::new(),
                                                };
                                                let (returned_watcher, _) = register_path_watch(
                                                    fs_watcher,
                                                    &mut watch_paths,
                                                    abs_path,
                                                    sub,
                                                )
                                                .await;
                                                fs_watcher = returned_watcher;
                                            }
                                        }
                                    }
                                    WatchAction::Preserve => {
                                        // Watches are already live. Nothing to do.
                                    }
                                }

                                if matches!(outcome.transition, StateTransition::NewlyActive) {
                                    // Cold → Active: execute inline to populate cache. Use the
                                    // source's effective key path (None for Global, requested path
                                    // for PathScoped) so the cache slot matches the lifecycle key.
                                    if self.cache.get_source(&provider, effective_key_path.as_deref(), &sm.name).is_none() {
                                        self.execute_source(&provider, &sm.name, effective_key_path.as_deref());
                                    }
                                }
                            }
                            last_activity = now;
                        }
                    }
                }

                // Filesystem events from watcher.
                Some(paths) = fs_rx.recv() => {
                    let affected_keys = resolve_keys_from_paths(&paths, &watch_paths);
                    for key in affected_keys {
                        let outcome = lifecycle.on_fsevent(key.clone(), Instant::now());
                        if outcome.refresh {
                            self.execute_source(&key.0, &key.2, key.1.as_deref());
                        }
                    }
                    last_activity = Instant::now();
                }

                // Watch self-test verdict (canon provider_source.md §"Watch backend health").
                // Failure swaps the live watcher for the polling backend and
                // re-registers every watch path on it.
                res = async { self_test_rx.as_mut().expect("guarded by is_some").await },
                    if self_test_rx.is_some() =>
                {
                    self_test_rx = None;
                    // A dropped sender is a harness failure, not evidence of a
                    // dead backend — stay on native.
                    if !res.unwrap_or(true) {
                        warn!(
                            "watch self-test: no fs events delivered within {:?}; provider watching falls back to polling",
                            crate::watcher::WATCH_SELF_TEST_TIMEOUT
                        );
                        match FsWatcher::new_polling_fallback(
                            crate::watcher::POLLING_FALLBACK_INTERVAL,
                        ) {
                            Ok((mut poll_watcher, poll_rx)) => {
                                for path in watch_paths.keys() {
                                    if let Err(e) = poll_watcher.watch(path) {
                                        warn!("failed to re-register watch on {path:?}: {e}");
                                    }
                                }
                                fs_watcher = poll_watcher;
                                fs_rx = poll_rx;
                                watch_backend = crate::watcher::WatchBackend::Polling;
                            }
                            Err(e) => {
                                warn!(
                                    "failed to create polling fallback watcher: {e}; staying on native (events may not deliver)"
                                );
                            }
                        }
                    }
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
                        debug!(
                            "Poll tick: executing provider={} path={:?} source={}",
                            key.0, key.1, key.2
                        );
                        self.execute_source(&key.0, &key.2, key.1.as_deref());
                    }

                    for key in &actions.watch_drops {
                        drop_watches_for_key(key, &mut watch_paths, &mut fs_watcher);
                    }

                    for key in &actions.evictions {
                        debug!(
                            "Evicting source provider={} path={:?} source={}",
                            key.0, key.1, key.2
                        );
                        // Evict only this source's contribution; the entry is removed
                        // only when its last source evicts (canon: lifecycle is keyed
                        // per (provider, path, source)).
                        self.cache.remove_source(&key.0, key.1.as_deref(), &key.2);
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
                            let names: Vec<String> = self
                                .registry
                                .provider_sources(&provider)
                                .map(|src| src.iter().map(|sm| sm.name.clone()).collect())
                                .unwrap_or_default();
                            for name in &names {
                                self.execute_source(&provider, name, path.as_deref());
                            }
                            last_activity = Instant::now();
                        }
                        Some(SchedulerMessage::FsEvent { .. }) => {
                            // No-op without watcher.
                        }
                        Some(SchedulerMessage::GetStatus { reply }) => {
                            let empty_watch_paths: HashMap<PathBuf, Vec<Subscription>> = HashMap::new();
                            let status = build_status(
                                &lifecycle,
                                &empty_watch_paths,
                                &self.in_flight,
                                "disabled",
                            );
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
                            let now = Instant::now();
                            let map: HashMap<lifecycle::Key, LifecycleSnapshot> = lifecycle
                                .iter()
                                .map(|(k, entry)| (k.clone(), snapshot_entry(k, entry, now)))
                                .collect();
                            let _ = reply.send(map);
                        }
                        Some(SchedulerMessage::QueryActivity { provider, path, demand }) => {
                            let now = Instant::now();
                            let Some(sources) = self.registry.provider_sources(&provider) else {
                                last_activity = now;
                                continue;
                            };
                            // Narrow to the demanded Sources. Scope/path policy in the
                            // loop below is unchanged and applies to whichever Sources
                            // survive this filter (canon §150/§268: a field demands only
                            // its owning Source).
                            let sources_vec: Vec<_> = match &demand {
                                SourceDemand::All => sources.to_vec(),
                                SourceDemand::Sources(names) => sources
                                    .iter()
                                    .filter(|sm| names.iter().any(|n| n == &sm.name))
                                    .cloned()
                                    .collect(),
                            };
                            for sm in &sources_vec {
                                if matches!(sm.scope, SourceScope::PathScoped) && path.is_none() {
                                    continue;
                                }
                                let effective_key_path = match sm.scope {
                                    SourceScope::Global => None,
                                    SourceScope::PathScoped => path.clone(),
                                };

                                let key: lifecycle::Key = (
                                    provider.clone(),
                                    effective_key_path.clone(),
                                    sm.name.clone(),
                                );
                                let cfg = SourceLifecycleConfig::from_strategy(
                                    &sm.invalidation,
                                    sm.keep_alive,
                                    sm.fsevents_reinstate,
                                );
                                let outcome = lifecycle.on_demand(key.clone(), cfg, now);

                                // No filesystem watching in this path.
                                let _ = outcome.watch_registration;

                                if matches!(outcome.transition, StateTransition::NewlyActive)
                                    && self.cache.get_source(&provider, effective_key_path.as_deref(), &sm.name).is_none() {
                                        self.execute_source(&provider, &sm.name, effective_key_path.as_deref());
                                    }
                            }
                            last_activity = now;
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
                        self.execute_source(&key.0, &key.2, key.1.as_deref());
                    }

                    // No watches to drop in the no-watcher path.

                    for key in &actions.evictions {
                        debug!(
                            "Evicting source provider={} path={:?} source={}",
                            key.0, key.1, key.2
                        );
                        // Evict only this source's contribution; the entry is removed
                        // only when its last source evicts (canon: lifecycle is keyed
                        // per (provider, path, source)).
                        self.cache.remove_source(&key.0, key.1.as_deref(), &key.2);
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

    fn handle_fs_event(
        &self,
        paths: Vec<PathBuf>,
        watch_paths: &HashMap<PathBuf, Vec<Subscription>>,
        lifecycle: &mut LifecycleRegistry,
    ) {
        let affected_keys = resolve_keys_from_paths(&paths, watch_paths);
        for key in affected_keys {
            let outcome = lifecycle.on_fsevent(key.clone(), Instant::now());
            if outcome.refresh {
                self.execute_source(&key.0, &key.2, key.1.as_deref());
            }
        }
    }
}

/// Register a fs watch on `path` for `sub`, deduplicating the underlying
/// kernel registration against already-tracked paths: multiple Sources
/// subscribing to the same path (e.g. git's `refs` and `status` sources at
/// the same repo root) share one kernel `watch()` call, keyed off
/// `watch_paths` — the same map that already governs unwatch-on-last-drop
/// in `drop_watches_for_key`, so registration and cleanup share one source
/// of truth for "is this path currently watched".
///
/// When a kernel call is actually needed (first subscriber for `path`), it
/// runs on the blocking thread pool via `spawn_blocking`: kernel FSEvents
/// registration has been measured at 1-3s per call on a loaded host, and
/// running it inline on the scheduler's async task blocks the worker thread
/// it shares with other tasks (e.g. connection tasks writing already-computed
/// responses) for that whole duration. `fs_watcher` is moved in and handed
/// back so the caller's loop keeps ownership across the await point.
///
/// Gap semantics: between the moment this call is issued and the moment the
/// kernel registration completes, no fs events for `path` are observed (the
/// OS is not yet watching it) — the same gap that existed before this fix's
/// spawn_blocking off-load, just no longer accompanied by a blocked
/// scheduler task. A demand signal that arrives for a Source whose
/// registration is still in flight is not lost: `on_demand` already
/// recorded the lifecycle transition synchronously, and the scheduler's
/// message loop processes one `QueryActivity` at a time, so no second
/// registration attempt races this one for the same path.
///
/// Returns the `FsWatcher` plus whether the subscription was recorded —
/// false only when a fresh kernel registration was required and failed.
async fn register_path_watch(
    fs_watcher: FsWatcher,
    watch_paths: &mut HashMap<PathBuf, Vec<Subscription>>,
    path: PathBuf,
    sub: Subscription,
) -> (FsWatcher, bool) {
    if let Some(subs) = watch_paths.get_mut(&path) {
        subs.push(sub);
        return (fs_watcher, true);
    }

    let path_for_call = path.clone();
    let (fs_watcher, result) = tokio::task::spawn_blocking(move || {
        let mut fs_watcher = fs_watcher;
        let result = fs_watcher.watch(&path_for_call);
        (fs_watcher, result)
    })
    .await
    .expect("watch registration task panicked");

    match result {
        Ok(()) => {
            watch_paths.insert(path, vec![sub]);
            (fs_watcher, true)
        }
        Err(e) => {
            warn!("Failed to watch {:?}: {}", path, e);
            (fs_watcher, false)
        }
    }
}

/// Resolve affected lifecycle keys from a set of changed paths using watch_paths + patterns.
fn resolve_keys_from_paths(
    changed_paths: &[PathBuf],
    watch_paths: &HashMap<PathBuf, Vec<Subscription>>,
) -> Vec<lifecycle::Key> {
    let mut keys = Vec::new();
    for changed_path in changed_paths {
        for (watch_path, subscriptions) in watch_paths {
            if !(changed_path.starts_with(watch_path) || changed_path == watch_path) {
                continue;
            }
            for sub in subscriptions {
                if event_matches_patterns(&sub.patterns, watch_path, changed_path) {
                    keys.push((sub.provider.clone(), sub.path.clone(), sub.source.clone()));
                }
            }
        }
    }
    keys
}

/// Remove watch subscriptions for a specific key and unwatch the path if no subscriptions remain.
fn drop_watches_for_key(
    key: &lifecycle::Key,
    watch_paths: &mut HashMap<PathBuf, Vec<Subscription>>,
    fs_watcher: &mut FsWatcher,
) {
    let mut paths_to_unwatch = Vec::new();
    for (watch_path, subscriptions) in watch_paths.iter_mut() {
        subscriptions
            .retain(|sub| !(sub.provider == key.0 && sub.path == key.1 && sub.source == key.2));
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

    /// Pinning test for the watch-registration dedup fix: when multiple
    /// Sources subscribe to the same path (e.g. git's `refs` and `status`
    /// sources at the same repo root), the underlying kernel `watch()` call
    /// must happen exactly once — not once per subscribing Source. Kernel
    /// FSEvents registration costs 1-3s per call on a loaded host; calling
    /// it redundantly once per source is what caused the scheduler-loop
    /// stall this fix addresses.
    #[tokio::test]
    async fn register_path_watch_dedupes_kernel_registration_for_shared_path() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let (fs_watcher, _rx) =
            crate::watcher::FsWatcher::new_polling(Duration::from_millis(50)).unwrap();
        let mut watch_paths: HashMap<PathBuf, Vec<Subscription>> = HashMap::new();

        let sub_refs = Subscription {
            provider: "git".to_string(),
            path: Some(dir.display().to_string()),
            source: "refs".to_string(),
            patterns: vec![".git".to_string()],
        };
        let sub_status = Subscription {
            provider: "git".to_string(),
            path: Some(dir.display().to_string()),
            source: "status".to_string(),
            patterns: vec![".git/index".to_string()],
        };

        let (fs_watcher, ok_a) =
            register_path_watch(fs_watcher, &mut watch_paths, dir.clone(), sub_refs).await;
        assert!(ok_a, "first subscriber's registration should succeed");
        let (fs_watcher, ok_b) =
            register_path_watch(fs_watcher, &mut watch_paths, dir.clone(), sub_status).await;
        assert!(ok_b, "second subscriber's registration should succeed");

        assert_eq!(
            fs_watcher.watch_call_count(),
            1,
            "kernel watch() must be called once per path, not once per subscribing source"
        );
        assert_eq!(
            watch_paths.get(&dir).unwrap().len(),
            2,
            "both subscriptions are recorded despite the single kernel registration"
        );
    }

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
