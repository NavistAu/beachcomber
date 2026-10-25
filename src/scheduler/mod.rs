pub mod lifecycle;

#[cfg(test)]
mod backoff_tests;
#[cfg(test)]
mod canon_provider_source_tests;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::time::{Duration, interval};
use tracing::{debug, info, warn};

use crate::query::Instance;

use crate::cache::Cache;
pub use crate::cache::FailureSnapshot;
use crate::config::Config;
use crate::provider::InvalidationStrategy;
use crate::provider::registry::{ProviderRegistry, RegistryHandle};
use crate::provider::source_call::record_outcome;
use crate::scheduler::lifecycle::{
    LifecycleRegistry, LifecycleState, SourceLifecycleConfig, StateTransition, WatchAction,
};
use crate::watcher::FsWatcher;
use crate::watcher_registry::WatcherRegistry;

/// Messages sent from the Server to the Scheduler.
#[derive(Debug)]
pub enum SchedulerMessage {
    /// An out-of-band re-execution of these instances: each Source the key
    /// demands, at the root that answers for the requested directory.
    Refresh {
        provider: String,
        instances: Vec<Instance>,
    },
    FsEvent {
        paths: Vec<PathBuf>,
    },
    Shutdown,
    /// Request scheduler status info. Response sent via oneshot channel.
    GetStatus {
        reply: tokio::sync::oneshot::Sender<SchedulerStatus>,
    },
    /// A provider was queried via get/watch. Signals demand for every instance
    /// the read reached, to keep them warm: one `(source, root)` per Source the
    /// key demands and root its chain visited. The first demand of an instance
    /// creates its lifecycle entry, and with it its schedule.
    QueryActivity {
        provider: String,
        instances: Vec<Instance>,
    },
    /// A `put` wrote at `node`. The put's lifecycle is its ttl: `Some(N)` makes
    /// the address a lifecycle entry (poll N x 1, decay 1), replacing every put
    /// entry at or below `node`; `None` just removes them. Reads never renew it.
    PutWritten {
        provider: String,
        path: Option<String>,
        node: String,
        ttl_secs: Option<u64>,
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
    /// The configuration changed under `providers` and `next` is the registry
    /// built from it. The scheduler adopts it, brings its lifecycle entries to
    /// match and then puts it in force: see [`Scheduler::reload`].
    Reload {
        next: Arc<ProviderRegistry>,
        /// Where requests are served from: the scheduler puts `next` in force
        /// there once its own state matches it, so no request is served by a
        /// registry the scheduler has not caught up with.
        registry: RegistryHandle,
    },
}

/// A point-in-time snapshot of a single lifecycle registry entry.
#[derive(Debug, Clone)]
pub struct LifecycleSnapshot {
    /// 0 = Active, 1–4 = Decay step.
    pub decay: u8,
    /// The entry's decay depth: steps from Active to eviction.
    pub depth: u8,
    /// Effective poll interval in seconds (may be doubled during decay). 0 for pure Watch.
    pub poll_interval_secs: u64,
    /// The `count` of `poll(interval, count)`: polls in the Active window.
    /// `None` for pure Watch (no poll of its own).
    pub poll_count: Option<u32>,
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

/// What a run cut off at its `timeout` reports, wherever it ran.
pub fn timed_out(timeout: Duration) -> String {
    format!(
        "timed out after {}",
        libbeachcomber::render::duration(timeout)
    )
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

    /// [`Self::send`] for a thread that is not on the runtime (the config
    /// watch's), waiting for room in the queue.
    pub fn send_blocking(&self, msg: SchedulerMessage) {
        let _ = self.tx.blocking_send(msg);
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
    /// Missing keys are not in the lifecycle registry (e.g. static/put entries).
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
pub(crate) struct FailureState {
    consecutive_failures: u32,
    suppressed_until: Option<Instant>,
    threshold: u32,
    backoff_interval: Duration,
}

impl FailureState {
    pub(crate) fn new(threshold: u32, backoff_interval: Duration) -> Self {
        Self {
            consecutive_failures: 0,
            suppressed_until: None,
            threshold,
            backoff_interval,
        }
    }

    pub(crate) fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= self.threshold {
            // 4 levels of exponential backoff from base interval, stays at level 4
            let level = (self.consecutive_failures - self.threshold).min(3);
            let delay = self.backoff_interval * (1u32 << level);
            self.suppressed_until = Some(Instant::now() + delay);
        }
    }

    pub(crate) fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.suppressed_until = None;
    }

    pub(crate) fn is_suppressed(&self) -> bool {
        self.suppressed_until
            .map(|until| Instant::now() < until)
            .unwrap_or(false)
    }
}

/// Build a `LifecycleSnapshot` for a given lifecycle entry at `now`. Centralised
/// so the two snapshot-reply sites (sync and async branches in the scheduler
/// loop) compute polls_elapsed and poll_count identically.
fn snapshot_entry(
    key: &lifecycle::Key,
    entry: &lifecycle::LifecycleEntry,
    now: Instant,
) -> LifecycleSnapshot {
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

    let step_duration_secs = entry.config.lifespan().as_secs() * rate_mult;
    let poll_count = entry.config.poll_interval.map(|_| entry.config.poll_count);

    // polls_elapsed: how many polls have fired in the current lifecycle step.
    // Only meaningful when there is a poll path.
    let polls_elapsed = match (poll_count, poll_interval_secs) {
        (Some(count), p) if p > 0 && step_duration_secs > 0 => {
            let step_start = entry
                .decay_timer
                .step_deadline
                .checked_sub(Duration::from_secs(step_duration_secs))
                .unwrap_or(now);
            let secs_in_step = now.saturating_duration_since(step_start).as_secs();
            let n = secs_in_step / p;
            (n as u32).min(count.max(1))
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
        depth: entry.config.decay,
        poll_interval_secs,
        poll_count,
        polls_elapsed,
        next_poll_in_secs,
        fsevents_reinstate: entry.config.fsevents_reinstate,
        watches_files,
        source: key.source.clone(),
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
        .map(|key| {
            let mut name = match &key.path {
                Some(pa) => format!("{}:{pa}:{}", key.provider, key.source),
                None => format!("{}:{}", key.provider, key.source),
            };
            if !key.node.is_empty() {
                name.push('.');
                name.push_str(&key.node);
            }
            name
        })
        .collect();

    let mut backoff_info: Vec<LifecycleInfo> = Vec::new();
    let mut poll_timer_info: Vec<PollTimerInfo> = Vec::new();
    let mut demand_info: Vec<DemandInfo> = Vec::new();

    for (key, entry) in lifecycle.iter() {
        let (provider, path, source) = (&key.provider, &key.path, &key.source);
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
                demand_info.push(DemandInfo {
                    provider: provider.clone(),
                    path: path.clone(),
                    source: source.clone(),
                    last_query_secs_ago: entry.decay_timer.last_demand.elapsed().as_secs(),
                });
            }
            LifecycleState::Decay(n) => {
                // Decaying entries go in the lifecycle list.
                let elapsed_secs = entry.decay_timer.last_demand.elapsed().as_secs();
                backoff_info.push(LifecycleInfo {
                    provider: provider.clone(),
                    path: path.clone(),
                    source: source.clone(),
                    stage: format!("Decay{n}"),
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
pub(crate) type SourceFailureMap = Arc<std::sync::Mutex<HashMap<lifecycle::Key, FailureState>>>;

/// Whether an event at `event_path` matches any of the watch `patterns` of an
/// instance rooted at `root`.
///
/// A pattern naming one file or directory (`.git`, `mise.toml`) matches any
/// path component at any depth: an event at `some/nested/.git/HEAD` matches
/// `.git`. This is intentional for monorepos with nested git submodules.
///
/// A pattern naming a path (`.git/HEAD`, `.terraform/environment`) is relative
/// to the root: it matches the event at that path and anything below it, and
/// nowhere else.
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
    patterns.iter().any(|pattern| {
        let pattern = pattern.trim_end_matches('/');
        if pattern.contains('/') {
            relative.starts_with(pattern)
        } else {
            relative
                .components()
                .any(|c| c.as_os_str().to_string_lossy() == pattern)
        }
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
    /// The runs in flight that a reload has overtaken: the Source is gone, its
    /// body changed or its schedule was retuned. Such a run writes nothing and
    /// goes back through the scheduler, so what it produces comes from the
    /// Source now in force. Marked, and read, under the `in_flight` lock.
    /// The value says whether the run goes again: a retuned instance does, an
    /// evicted one (its Source is gone or changed) has nothing to go back to.
    superseded: Arc<std::sync::Mutex<HashMap<lifecycle::Key, bool>>>,
    /// Where a run that must go again sends its `Refresh`. Weak, so the
    /// scheduler's own sender does not keep its queue open once every handle
    /// is gone.
    requeue: mpsc::WeakSender<SchedulerMessage>,
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
    /// When set, `run()` serves provider file-watching from the polling
    /// backend at this interval and skips the self-test. See
    /// `use_polling_watcher`.
    polling_watcher: Option<Duration>,
}

impl Scheduler {
    pub fn new(
        cache: Arc<Cache>,
        registry: Arc<ProviderRegistry>,
        config: Config,
        watchers: Arc<WatcherRegistry>,
    ) -> (SchedulerHandle, Scheduler) {
        let (tx, rx) = mpsc::channel(256);
        let requeue = tx.downgrade();
        let handle = SchedulerHandle::new(tx);
        let heartbeat = Arc::new(AtomicU64::new(0));
        let failure_counts = Arc::clone(&registry.failures);
        let scheduler = Scheduler {
            cache,
            registry,
            config,
            rx,
            in_flight: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            pending_rerun: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            superseded: Arc::default(),
            requeue,
            failure_counts,
            heartbeat,
            watchers,
            run_watch_self_test: false,
            polling_watcher: None,
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

    /// Re-execute `instances` of `provider`.
    fn refresh_sources(&self, provider: &str, instances: &[Instance]) {
        for instance in instances {
            self.execute_source(provider, &instance.source, instance.root.as_deref());
        }
    }

    /// Execute one instance of a source (`root` is its cache address, `None`
    /// for the instance at `~`) on the blocking thread pool and write the result
    /// to cache.
    /// This is fire-and-forget: returns immediately while the source runs in the background.
    /// Deduplicates concurrent executions via in_flight: if the (provider, root, source) triple
    /// is already running, marks it for a single rerun after completion.
    /// Suppresses execution when failure backoff is active.
    fn execute_source(&self, provider_name: &str, source_name: &str, root: Option<&str>) {
        let Some(source) = self.registry.source(provider_name, source_name) else {
            warn!(
                "Refresh for unknown source '{}.{}'",
                provider_name, source_name
            );
            return;
        };

        let key = lifecycle::Key {
            provider: provider_name.to_string(),
            path: root.map(|s| s.to_string()),
            source: source_name.to_string(),
            node: String::new(),
        };

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
        // The Source's own timeout (its `timeout(...)`, else `[daemon]
        // timeout`, applied when the registry was built).
        let timeout = self
            .registry
            .source_metadata(provider_name, source_name)
            .map_or_else(|| self.config.daemon().timeout(), |m| m.timeout);
        let in_flight = Arc::clone(&self.in_flight);
        let pending_rerun = Arc::clone(&self.pending_rerun);
        let superseded = Arc::clone(&self.superseded);
        let requeue = self.requeue.clone();
        // The run reads through the registry it began under, which its Source's
        // `comb.<p>` reader holds only weakly: kept here, a reload mid-run
        // cannot leave it reading a registry that has gone.
        let registry = Arc::clone(&self.registry);
        // And settles under the same one: a reload that retires the Source
        // marks it, whichever registry has since taken over.
        let recording_registry = Arc::clone(&registry);
        // `root` is the cache address; the directory the Source runs in is not
        // always the same string. The global entry's address is `None` and its
        // directory is `$HOME` (`query::normalise_path`), so the instance at
        // `~` runs in `$HOME` and writes the pathless entry.
        let exec_dir = crate::query::instance_dir(root);
        let provider_name_owned = provider_name.to_string();
        let source_name_owned = source_name.to_string();

        tokio::spawn(async move {
            let result = tokio::time::timeout(
                timeout,
                tokio::task::spawn_blocking(move || {
                    let _registry = registry;
                    source.execute(exec_dir.as_deref())
                }),
            )
            .await;

            // Settle under the `in_flight` lock, the one a reload marks retuned
            // and evicted runs under: either this run is overtaken and writes
            // nothing, or it is recorded like any other run. A run whose Source
            // a reload has retired is dropped by `record_outcome`, whether or not
            // the instance had a lifecycle entry to mark.
            let again = {
                let mut flight = in_flight.lock().unwrap();
                let overtaken = superseded.lock().unwrap().remove(&key);
                let overtaken_rerun = overtaken == Some(true);
                let overtaken = overtaken.is_some();
                if overtaken {
                    debug!(
                        "Source '{}.{}' run superseded by a reload; result dropped",
                        provider_name_owned, source_name_owned
                    );
                } else {
                    let outcome = match result {
                        Ok(Ok(Ok(source_result))) => Ok(source_result),
                        Ok(Ok(Err(e))) => Err(e),
                        Ok(Err(e)) => Err(format!("panicked: {e}")),
                        Err(_) => Err(timed_out(timeout)),
                    };
                    match record_outcome(
                        &recording_registry,
                        &cache,
                        &provider_name_owned,
                        &source_name_owned,
                        key.path.as_deref(),
                        outcome,
                    ) {
                        Ok(()) => debug!(
                            "Executed source '{}.{}' path={:?}",
                            provider_name_owned, source_name_owned, key.path
                        ),
                        Err(e) => warn!(
                            "Source '{}.{}' failed: {}",
                            provider_name_owned, source_name_owned, e
                        ),
                    }
                }
                // A refresh that arrived during the run asks for one more; so
                // does an overtaken run. Either goes back through the
                // scheduler, which applies the failure backoff and runs the
                // Source in force. The key is free again before it is asked.
                let queued = pending_rerun.lock().unwrap().remove(&key);
                flight.remove(&key);
                // An evicted instance has no lifecycle entry to run under: its
                // result, and any refresh queued behind it, are dropped.
                if overtaken { overtaken_rerun } else { queued }
            };
            if again && let Some(tx) = requeue.upgrade() {
                debug!("Re-running source '{}.{}'", key.provider, key.source);
                let _ = tx
                    .send(SchedulerMessage::Refresh {
                        provider: key.provider.clone(),
                        instances: vec![Instance::new(&key.source, key.path.as_deref())],
                    })
                    .await;
            }
        });
    }

    /// Adopt `next` as this scheduler's registry, bring the lifecycle entries
    /// to match it and then put it in force in `registry`, where requests are
    /// served from. Returns the watches the caller must drop and register.
    ///
    /// - A Source that no longer exists, whose provider is now disabled or
    ///   whose body changed (its `root`, `data` and `result` lines, `_`
    ///   prototypes included) is evicted: its records and stored `:cache` keys
    ///   go, with its lifecycle entry, the roots and callees kept for it, its
    ///   failure state and its watches. A Source that still exists is run by
    ///   the next read that asks for it.
    /// - A Source whose schedule changed (invalidation, failback, decay,
    ///   budget, nearest, or the decay depth that applies to it) is retuned in
    ///   place: its entry keeps its state and takes the new configuration, new
    ///   watches are registered, and the schedule stored with its records is
    ///   restamped, with no run.
    /// - Every other instance keeps its records, entry, timers and watches.
    ///
    /// A run in flight for an evicted or retuned instance is marked
    /// superseded: it writes nothing and goes again under the new Source. Puts
    /// are not Sources and are never touched.
    fn reload(
        &mut self,
        next: Arc<ProviderRegistry>,
        registry: &RegistryHandle,
        lifecycle: &mut LifecycleRegistry,
    ) -> ReloadWatches {
        let previous = std::mem::replace(&mut self.registry, next);
        let plan = plan_reload(&self.cache, lifecycle, &previous, &self.registry);
        // Mark first: a request-path run that began under `previous` and has
        // not written yet is skipped from here on, and one that is writing
        // finishes before the marks are made, so the eviction below removes it.
        previous.retire(&self.registry, &plan.affected);
        let mut watches = ReloadWatches::default();

        // Under the `in_flight` lock, so a run settling concurrently either
        // wrote before this or is overtaken by it.
        {
            let flight = self.in_flight.lock().unwrap();
            let mut superseded = self.superseded.lock().unwrap();
            let mut failures = self.failure_counts.lock().unwrap();
            for key in &plan.evict {
                debug!(
                    "Reload: evicting provider={} path={:?} source={}",
                    key.provider, key.path, key.source
                );
                if flight.contains(key) {
                    superseded.insert(key.clone(), false);
                }
                lifecycle.remove(key);
                on_evicted(&self.cache, &self.registry, key);
                watches.drop.push(key.clone());
            }
            for (provider, source) in &plan.affected {
                // Every instance, the ones with no lifecycle entry too.
                self.cache.remove_source_everywhere(provider, source);
                self.registry.forget_callees_of(provider, source);
            }
            for retune in &plan.retune {
                debug!(
                    "Reload: retuning provider={} path={:?} source={}",
                    retune.key.provider, retune.key.path, retune.key.source
                );
                if flight.contains(&retune.key) {
                    superseded.insert(retune.key.clone(), true);
                }
                lifecycle.retune(&retune.key, retune.config.clone());
                if retune.stored_changed
                    && let Some(sm) = self
                        .registry
                        .source_metadata(&retune.key.provider, &retune.key.source)
                {
                    self.cache.restamp_schedule(
                        &retune.key.provider,
                        retune.key.path.as_deref(),
                        &retune.key.source,
                        crate::cache::Schedule::of(sm),
                    );
                }
                if retune.failback_changed {
                    failures.remove(&retune.key);
                }
                if retune.watches_changed {
                    watches.drop.push(retune.key.clone());
                    if retune.rewatch {
                        watches.rewatch.push(retune.key.clone());
                    }
                }
            }
            // The failure state of a Source that is gone or changed: a fixed
            // body is not held back by the backoff the old one earned.
            failures.retain(|key, _| {
                key.source == crate::cache::PUT_SOURCE
                    || !plan
                        .affected
                        .contains(&(key.provider.clone(), key.source.clone()))
            });
        }
        registry.replace(Arc::clone(&self.registry));
        watches
    }

    /// Opt this scheduler into the startup watch self-test (canon
    /// provider_source.md §"Watch backend health"). The daemon path sets this; directly-constructed
    /// schedulers (tests) default to assuming the native backend, exactly the
    /// pre-self-test behaviour, and skip the probe and its 500ms worst case.
    pub fn self_test_watch_backend(&mut self) {
        self.run_watch_self_test = true;
    }

    /// Diagnostic/test seam: poll for file changes at `interval` instead of
    /// registering kernel watches, and skip the watch self-test. Kernel
    /// FSEvents registration costs ~1.2s per `watch()` call on macOS, paid by
    /// every daemon start whether or not the caller is about file-watching.
    pub fn use_polling_watcher(&mut self, interval: Duration) {
        self.polling_watcher = Some(interval);
        self.run_watch_self_test = false;
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
        let watcher = match self.polling_watcher {
            Some(interval) => FsWatcher::new_polling(interval),
            None => FsWatcher::new(),
        };
        // Without a watcher the loop still serves demand, polls and reloads;
        // nothing is watched, and `_idle_events` keeps the event channel open
        // and silent.
        let (mut fs_watcher, mut fs_rx, _idle_events) = match watcher {
            Ok((watcher, rx)) => (Some(watcher), rx, None),
            Err(e) => {
                warn!(
                    "Failed to create filesystem watcher: {}. Watch triggers disabled.",
                    e
                );
                let (tx, rx) = mpsc::channel::<Vec<PathBuf>>(1);
                (None, rx, Some(tx))
            }
        };
        let mut watch_backend = fs_watcher.as_ref().map(|_| {
            if self.polling_watcher.is_some() {
                crate::watcher::WatchBackend::Polling
            } else {
                crate::watcher::WatchBackend::Native
            }
        });

        let mut self_test_rx: Option<tokio::sync::oneshot::Receiver<bool>> =
            if self.run_watch_self_test && fs_watcher.is_some() {
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
        let idle_shutdown = self.config.daemon().idle_shutdown();

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
                        Some(SchedulerMessage::Refresh { provider, instances }) => {
                            debug!("Refresh: provider={} instances={:?}", provider, instances);
                            self.refresh_sources(&provider, &instances);
                            last_activity = Instant::now();
                        }
                        Some(SchedulerMessage::PutWritten { provider, path, node, ttl_secs }) => {
                            apply_put_written(&mut lifecycle, &provider, path.as_deref(), &node, ttl_secs, Instant::now());
                            last_activity = Instant::now();
                        }
                        Some(SchedulerMessage::Reload { next, registry }) => {
                            let watches = self.reload(next, &registry, &mut lifecycle);
                            for key in &watches.drop {
                                drop_watches_for_key(key, &mut watch_paths, fs_watcher.as_mut());
                            }
                            for key in &watches.rewatch {
                                let Some(sm) = self.registry.source_metadata(&key.provider, &key.source) else {
                                    continue;
                                };
                                fs_watcher = watch_instance(
                                    fs_watcher,
                                    &mut watch_paths,
                                    &self.registry,
                                    &key.provider,
                                    sm,
                                    key.path.as_deref(),
                                )
                                .await;
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
                                watch_backend.map_or("disabled", |b| b.as_str()),
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
                        Some(SchedulerMessage::QueryActivity { provider, instances }) => {
                            let now = Instant::now();
                            // A demand for an instance is a demand for the
                            // instances it calls.
                            let demanded = self.registry.with_callees(&provider, &instances);
                            for (provider, instance) in &demanded {
                                let Some(sm) = self.registry.source_metadata(provider, &instance.source) else {
                                    continue;
                                };
                                let root = instance.root.clone();
                                let key = lifecycle::Key {
                                    provider: provider.clone(),
                                    path: root.clone(),
                                    source: sm.name.clone(),
                                    node: String::new(),
                                };
                                let outcome = lifecycle.demand(
                                    key.clone(),
                                    || source_lifecycle_config(&self.cache, provider, root.as_deref(), sm),
                                    now,
                                );

                                // The instance's schedule registers when it is
                                // first requested (and again when decay dropped
                                // its watches and a demand reinstates them),
                                // never at load.
                                if matches!(
                                    outcome.watch_registration,
                                    WatchAction::Register | WatchAction::Reinstate
                                ) {
                                    fs_watcher = watch_instance(
                                        fs_watcher,
                                        &mut watch_paths,
                                        &self.registry,
                                        provider,
                                        sm,
                                        root.as_deref(),
                                    )
                                    .await;
                                }

                                if matches!(outcome.transition, StateTransition::NewlyActive) {
                                    // Cold → Active: execute to populate the cache,
                                    // at the instance the lifecycle key names.
                                    if self.cache.get_source(provider, root.as_deref(), &sm.name).is_none() {
                                        self.execute_source(provider, &sm.name, root.as_deref());
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
                            self.execute_source(&key.provider, &key.source, key.path.as_deref());
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
                                fs_watcher = Some(poll_watcher);
                                fs_rx = poll_rx;
                                watch_backend = Some(crate::watcher::WatchBackend::Polling);
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
                    let actions = lifecycle.tick_holding(Instant::now(), &self.registry.callers_of());

                    for key in &actions.polls_due {
                        debug!(
                            "Poll tick: executing provider={} path={:?} source={}",
                            key.provider, key.path, key.source
                        );
                        if key.source == crate::cache::PUT_SOURCE {
                            // A put's poll is its expiry clock; nothing to run.
                            continue;
                        }
                        self.execute_source(&key.provider, &key.source, key.path.as_deref());
                    }

                    for key in &actions.watch_drops {
                        drop_watches_for_key(key, &mut watch_paths, fs_watcher.as_mut());
                    }

                    for key in &actions.evictions {
                        debug!(
                            "Evicting source provider={} path={:?} source={}",
                            key.provider, key.path, key.source
                        );
                        // Evict only this source's contribution; the entry is removed
                        // only when its last source evicts (canon: lifecycle is keyed
                        // per (provider, path, source)).
                        on_evicted(&self.cache, &self.registry, key);
                        drop_watches_for_key(key, &mut watch_paths, fs_watcher.as_mut());
                    }

                    for (key, new_state) in &actions.transitions {
                        debug!("lifecycle: key {:?} -> {:?}", key, new_state);
                    }

                    // Idle shutdown.
                    if let Some(idle) = idle_shutdown
                        && self.cache.is_empty()
                        && lifecycle.is_empty()
                        && last_activity.elapsed() >= idle
                    {
                        info!(
                            "Idle shutdown: no entries, idle for {}",
                            libbeachcomber::render::duration(idle)
                        );
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
                self.execute_source(&key.provider, &key.source, key.path.as_deref());
            }
        }
    }
}

/// Register the watches of one instance of `provider`'s Source `sm` at `root`
/// ([`instance_watches`]), and hand the watcher back.
async fn watch_instance(
    fs_watcher: Option<FsWatcher>,
    watch_paths: &mut HashMap<PathBuf, Vec<Subscription>>,
    registry: &ProviderRegistry,
    provider: &str,
    sm: &crate::provider::SourceMetadata,
    root: Option<&str>,
) -> Option<FsWatcher> {
    // No watcher, nothing to watch with.
    let mut fs_watcher = fs_watcher?;
    let src = registry.source(provider, &sm.name);
    for (watch_path, sub) in instance_watches(provider, sm, root, src.as_deref()) {
        let (returned_watcher, registered) =
            register_path_watch(fs_watcher, watch_paths, watch_path.clone(), sub).await;
        fs_watcher = returned_watcher;
        if registered {
            debug!(
                "Watching {:?} for provider={} source={} root={:?}",
                watch_path, provider, sm.name, root
            );
        }
    }
    Some(fs_watcher)
}

/// The watches an instance of a Source registers when it is first requested:
/// each as the path to watch and the subscription that routes its events to the
/// instance's lifecycle key.
///
/// A relative `watch` entry is a pattern under the instance's root directory,
/// so it registers only for a rooted instance: at `~` a recursive watch of
/// `$HOME` is the firehose, and the instance refreshes by poll, by demand and
/// (when read-always) on every read. An absolute entry (`/…`, or `~`/`$XDG_*`
/// expanded by the Source) is watched whatever the root, and an event on it
/// refreshes this instance. A Poll Source registers no watch at all: an empty
/// pattern list matches every event under its root.
fn instance_watches(
    provider: &str,
    sm: &crate::provider::SourceMetadata,
    root: Option<&str>,
    source: Option<&dyn crate::provider::Source>,
) -> Vec<(PathBuf, Subscription)> {
    let mut out = Vec::new();
    if matches!(sm.invalidation, InvalidationStrategy::Poll { .. }) {
        return out;
    }
    let subscription = |patterns: Vec<String>| Subscription {
        provider: provider.to_string(),
        path: root.map(str::to_string),
        source: sm.name.clone(),
        patterns,
    };
    let patterns = crate::provider::watch_patterns(&sm.invalidation);
    if let Some(root) = root {
        if !patterns.is_empty() {
            out.push((PathBuf::from(root), subscription(patterns.clone())));
        }
        // A source whose root is not itself a watchable directory names the
        // concrete files to watch via watched_files().
        if let Some(source) = source {
            for file in source.watched_files(Some(root)) {
                out.push((file, subscription(Vec::new())));
            }
        }
    }
    // The instance at `~` has no directory to watch recursively: that would be
    // all of `$HOME`. Its files are the single-component patterns joined to
    // `$HOME` (`mise.toml`, `.tool-versions`), each watched on its own.
    if root.is_none()
        && let Some(home) = crate::query::home_dir()
    {
        for pattern in &patterns {
            let file = home.join(pattern);
            if !pattern.contains('/') && file.exists() {
                out.push((file, subscription(Vec::new())));
            }
        }
    }
    for abs in crate::provider::watch_abs_paths(&sm.invalidation) {
        out.push((PathBuf::from(abs), subscription(Vec::new())));
    }
    out
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

/// Apply a `PutWritten`: drop every `put` lifecycle key at or below `node`,
/// then, when the put has a ttl, make the address a lifecycle entry: poll
/// `ttl x 1`, decay depth 1. Valid for the ttl, readable but stale for one
/// decay step, then evicted.
fn apply_put_written(
    lifecycle: &mut LifecycleRegistry,
    provider: &str,
    path: Option<&str>,
    node: &str,
    ttl_secs: Option<u64>,
    now: Instant,
) {
    lifecycle.remove_put_keys(provider, path, node);
    let Some(ttl_secs) = ttl_secs else {
        return;
    };
    let key = lifecycle::Key {
        provider: provider.to_string(),
        path: path.map(str::to_string),
        source: crate::cache::PUT_SOURCE.to_string(),
        node: node.to_string(),
    };
    let cfg = SourceLifecycleConfig::from_strategy(
        &InvalidationStrategy::Poll {
            interval_secs: ttl_secs,
            count: 1,
        },
        false,
        1,
    );
    lifecycle.on_demand(key, cfg, now);
}

/// The lifecycle a Source of `provider` runs under: its own strategy, with the
/// decay depth it sets itself, else the one its provider's `_` prototype gives
/// at `path` (`Cache::resolve_decay`).
fn source_lifecycle_config(
    cache: &Cache,
    provider: &str,
    path: Option<&str>,
    sm: &crate::provider::SourceMetadata,
) -> SourceLifecycleConfig {
    SourceLifecycleConfig::from_strategy(
        &sm.invalidation,
        sm.fsevents_reinstate,
        sm.decay
            .unwrap_or_else(|| cache.resolve_decay(provider, path)),
    )
}

/// The watches a reload leaves to the run loop, which owns the watcher.
#[derive(Default)]
struct ReloadWatches {
    /// Keys whose watches go: the instances evicted, and the retuned ones
    /// whose invalidation changed.
    drop: Vec<lifecycle::Key>,
    /// Keys to watch afresh, under the Source now in force.
    rewatch: Vec<lifecycle::Key>,
}

/// An instance whose schedule changed and which is retuned in place.
struct Retune {
    key: lifecycle::Key,
    /// What the entry becomes.
    config: SourceLifecycleConfig,
    /// The schedule stored with its records (`poll`, `watch`, `decay`) differs.
    stored_changed: bool,
    /// The failback differs, so the failure state starts afresh.
    failback_changed: bool,
    /// What it is invalidated by differs.
    watches_changed: bool,
    /// Whether it should be watching now (it is Active, or keeps its watches
    /// through decay) rather than waiting for a demand to reinstate them.
    rewatch: bool,
}

/// What a reload does to the lifecycle entries: the instances whose Source is
/// gone or changed, and those only rescheduled.
struct ReloadPlan {
    /// The Sources, as (provider, source), that are gone or whose body
    /// changed, and the Sources that call them (transitively), whose stored
    /// values were computed from the old body.
    affected: std::collections::HashSet<(String, String)>,
    evict: Vec<lifecycle::Key>,
    retune: Vec<Retune>,
}

/// Whether two metadata of one Source schedule it alike, in everything the
/// scheduler reads of it when it creates an entry.
fn same_schedule(a: &crate::provider::SourceMetadata, b: &crate::provider::SourceMetadata) -> bool {
    a.invalidation == b.invalidation
        && a.failback == b.failback
        && a.fsevents_reinstate == b.fsevents_reinstate
        && a.decay == b.decay
        && a.budget_execute == b.budget_execute
        && a.nearest == b.nearest
}

/// Compare every Source instance in `lifecycle` as `previous` scheduled it with
/// what `next` says. Puts are not Sources and are skipped.
fn plan_reload(
    cache: &Cache,
    lifecycle: &LifecycleRegistry,
    previous: &ProviderRegistry,
    next: &ProviderRegistry,
) -> ReloadPlan {
    let mut plan = ReloadPlan {
        affected: previous.retired_by(next),
        evict: Vec::new(),
        retune: Vec::new(),
    };
    for (key, entry) in lifecycle.iter() {
        if key.source == crate::cache::PUT_SOURCE {
            continue;
        }
        let affected = plan
            .affected
            .contains(&(key.provider.clone(), key.source.clone()));
        let Some(now) = next
            .source_metadata(&key.provider, &key.source)
            .filter(|_| !affected)
        else {
            plan.evict.push(key.clone());
            continue;
        };
        let before = previous.source_metadata(&key.provider, &key.source);
        let config = source_lifecycle_config(cache, &key.provider, key.path.as_deref(), now);
        if before.is_some_and(|before| same_schedule(before, now)) && config == entry.config {
            continue;
        }
        plan.retune.push(Retune {
            key: key.clone(),
            stored_changed: before.is_none_or(|before| {
                crate::cache::Schedule::of(before) != crate::cache::Schedule::of(now)
            }),
            failback_changed: before.is_none_or(|before| before.failback != now.failback),
            watches_changed: before.is_none_or(|before| before.invalidation != now.invalidation),
            rewatch: entry.state == LifecycleState::Active || config.fsevents_reinstate,
            config,
        });
    }
    plan
}

/// Evict one lifecycle key: a put address takes its own subtree with it, a
/// Source takes its whole contribution.
fn evict(cache: &Cache, key: &lifecycle::Key) {
    if key.source == crate::cache::PUT_SOURCE {
        cache.evict_put(&key.provider, key.path.as_deref(), &key.node);
    } else {
        cache.remove_source(&key.provider, key.path.as_deref(), &key.source);
    }
}

/// An instance left the lifecycle: take its records, the roots kept for it and
/// what it was known to call.
fn on_evicted(cache: &Cache, registry: &ProviderRegistry, key: &lifecycle::Key) {
    evict(cache, key);
    registry.forget_roots(&key.provider, &key.source, key.path.as_deref());
    registry.forget_callees(&key.provider, &key.source, key.path.as_deref());
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
                    keys.push(lifecycle::Key {
                        provider: sub.provider.clone(),
                        path: sub.path.clone(),
                        source: sub.source.clone(),
                        node: String::new(),
                    });
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
    mut fs_watcher: Option<&mut FsWatcher>,
) {
    let mut paths_to_unwatch = Vec::new();
    for (watch_path, subscriptions) in watch_paths.iter_mut() {
        subscriptions.retain(|sub| {
            // A Subscription belongs to a Source, whose key has no node.
            !(sub.provider == key.provider
                && sub.path == key.path
                && sub.source == key.source
                && key.node.is_empty())
        });
        if subscriptions.is_empty() {
            paths_to_unwatch.push(watch_path.clone());
        }
    }
    for wp in paths_to_unwatch {
        watch_paths.remove(&wp);
        if let Some(fs_watcher) = fs_watcher.as_deref_mut() {
            let _ = fs_watcher.unwatch(&wp);
        }
        debug!("Unwatched path {:?} (no subscriptions remaining)", wp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn put_key(path: Option<&str>, node: &str) -> lifecycle::Key {
        lifecycle::Key {
            provider: "build".into(),
            path: path.map(str::to_string),
            source: crate::cache::PUT_SOURCE.into(),
            node: node.into(),
        }
    }

    fn put_keys(lifecycle: &LifecycleRegistry) -> Vec<String> {
        let mut keys: Vec<String> = lifecycle.iter().map(|(k, _)| k.node.clone()).collect();
        keys.sort();
        keys
    }

    /// A Source that sets its own decay (`ttl(...)` sets 1) keeps it over its
    /// provider's `_.decay`; one that sets none takes the prototype's.
    #[test]
    fn a_source_decay_beats_the_prototype() {
        let registry = crate::provider::registry::ProviderRegistry::with_defaults();
        let mut sm = registry.provider_sources("git").unwrap()[0].clone();
        let cache = crate::cache::Cache::new();
        cache
            .write_static(
                "git:cache",
                None,
                crate::cache::PUT_SOURCE,
                "_.decay",
                crate::provider::Value::Int(6),
                None,
            )
            .unwrap();
        assert_eq!(sm.decay, None);
        assert_eq!(source_lifecycle_config(&cache, "git", None, &sm).decay, 6);
        sm.decay = Some(1);
        assert_eq!(source_lifecycle_config(&cache, "git", None, &sm).decay, 1);
    }

    #[test]
    fn a_source_registers_with_its_providers_decay() {
        let registry = ProviderRegistry::with_defaults();
        let sm = registry.provider_sources("git").unwrap()[0].clone();
        let cache = Cache::new();
        assert_eq!(
            source_lifecycle_config(&cache, "git", None, &sm).decay,
            lifecycle::DEFAULT_DECAY
        );
        cache
            .write_static(
                "git:cache",
                None,
                crate::cache::PUT_SOURCE,
                "_.decay",
                crate::provider::Value::Int(2),
                None,
            )
            .unwrap();
        assert_eq!(source_lifecycle_config(&cache, "git", None, &sm).decay, 2);
        assert_eq!(
            source_lifecycle_config(&cache, "mise", None, &sm).decay,
            lifecycle::DEFAULT_DECAY,
            "another provider's prototype does not apply"
        );
        cache
            .write_static(
                "git:cache",
                Some("/w/proj"),
                crate::cache::PUT_SOURCE,
                "_.decay",
                crate::provider::Value::Int(6),
                None,
            )
            .unwrap();
        assert_eq!(
            source_lifecycle_config(&cache, "git", Some("/w/proj/sub"), &sm).decay,
            6
        );
        assert_eq!(
            source_lifecycle_config(&cache, "git", Some("/w/other"), &sm).decay,
            2
        );
    }

    /// Each put address is its own entry: valid for the ttl, stale for one
    /// decay step (twice the ttl), then evicted. Nothing but a put renews it.
    #[test]
    fn a_put_entry_is_not_renewed_by_reads() {
        let mut lifecycle = LifecycleRegistry::new();
        let t0 = Instant::now();
        apply_put_written(&mut lifecycle, "build", None, "status", Some(10), t0);

        // No read path touches a put key: the only API that renews an entry is
        // `on_demand`, and `apply_put_written` is its only put caller. The
        // ttl runs out regardless of what else happens.
        let actions = lifecycle.tick(t0 + Duration::from_secs(11));
        assert_eq!(
            actions.transitions.len(),
            1,
            "Active -> Decay(1) after the ttl"
        );
        assert!(actions.evictions.is_empty());
        let actions = lifecycle.tick(t0 + Duration::from_secs(10 + 20 + 1));
        assert_eq!(actions.evictions, vec![put_key(None, "status")]);

        // The next put starts it over.
        apply_put_written(&mut lifecycle, "build", None, "status", Some(10), t0);
        assert_eq!(put_keys(&lifecycle), vec!["status"]);
    }

    #[test]
    fn a_put_replaces_the_lifecycle_at_and_below_its_node_only() {
        let mut lifecycle = LifecycleRegistry::new();
        let t0 = Instant::now();
        for node in ["a", "a.b", "ab", "c"] {
            apply_put_written(&mut lifecycle, "build", None, node, Some(10), t0);
        }
        apply_put_written(&mut lifecycle, "build", Some("/p"), "a", Some(10), t0);

        // A put with no ttl at `a` drops `a` and `a.b`, not `ab`, `c` or the
        // same node at another path.
        apply_put_written(&mut lifecycle, "build", None, "a", None, t0);
        assert_eq!(put_keys(&lifecycle), vec!["a", "ab", "c"]);
        assert!(lifecycle.iter().any(|(k, _)| k.path.is_some()));

        // The root address covers every node at that (provider, path).
        apply_put_written(&mut lifecycle, "build", None, "", None, t0);
        assert_eq!(put_keys(&lifecycle), vec!["a"]);
    }

    #[test]
    fn evicting_a_put_key_removes_only_that_address() {
        use crate::cache::PUT_SOURCE;
        use crate::provider::Value;
        let cache = Cache::new();
        let write = |node: &str, v: i64| {
            cache
                .write_static("build", None, PUT_SOURCE, node, Value::Int(v), Some(10))
                .unwrap();
        };
        write("status", 1);
        write("eta", 2);

        evict(&cache, &put_key(None, "status"));

        let rows = cache.list_rows();
        let fields: Vec<&str> = rows.iter().map(|r| r.field.as_str()).collect();
        assert_eq!(fields, vec!["eta"]);
    }

    /// Each put address keeps the lifecycle its own put set: `build` expiring
    /// leaves `build.status`, which a deeper put owns.
    #[test]
    fn evicting_a_put_address_leaves_a_deeper_put_address() {
        use crate::cache::PUT_SOURCE;
        use crate::provider::Value;
        use std::collections::HashMap;
        let cache = Cache::new();
        cache
            .write_static(
                "build",
                None,
                PUT_SOURCE,
                "",
                Value::Object(HashMap::from([
                    ("eta".to_string(), Value::Int(1)),
                    ("status".to_string(), Value::String("old".into())),
                ])),
                Some(60),
            )
            .unwrap();
        cache
            .write_static(
                "build",
                None,
                PUT_SOURCE,
                "status",
                Value::String("x".into()),
                Some(300),
            )
            .unwrap();

        evict(&cache, &put_key(None, ""));

        let rows = cache.list_rows();
        let fields: Vec<&str> = rows.iter().map(|r| r.field.as_str()).collect();
        assert_eq!(fields, vec!["status"]);
    }

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
    fn a_multi_component_pattern_is_a_path_relative_to_the_root() {
        let root = PathBuf::from("/proj");
        let head = [".git/HEAD".to_string()];
        assert!(event_matches_patterns(
            &head,
            &root,
            Path::new("/proj/.git/HEAD")
        ));
        assert!(!event_matches_patterns(
            &head,
            &root,
            Path::new("/proj/.git/index")
        ));
        assert!(!event_matches_patterns(
            &head,
            &root,
            Path::new("/proj/sub/.git/HEAD")
        ));
        assert!(event_matches_patterns(
            &["a/b".to_string()],
            &root,
            Path::new("/proj/a/b/c.txt")
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

    /// An instance that leaves the lifecycle takes with it what it was known
    /// to call, and an instance of the same Source elsewhere keeps its own.
    #[test]
    fn an_evicted_instance_forgets_what_it_called() {
        let config = crate::config::Config::from_toml(
            "[daemon.providers.p.sources.b]\n\
             data = \"{{ 1 }}\"\n\
             result.vb = \"{{ data }}\"\n\
             [daemon.providers.p.sources.a]\n\
             data = \"{{ sources.b().vb }}\"\n\
             result.va = \"{{ data }}\"\n",
        );
        let cache = Arc::new(Cache::new());
        let registry = ProviderRegistry::with_config(&config, &Default::default())
            .unwrap()
            .share(&cache);
        let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a = registry.source("p", "a").unwrap();
        let mut keys = Vec::new();
        for dir in [&one, &two] {
            let at = dir.path().to_str().unwrap();
            a.execute(Some(at)).unwrap();
            keys.push(lifecycle::Key {
                provider: "p".into(),
                path: crate::query::address_of(at),
                source: "a".into(),
                node: String::new(),
            });
        }
        assert_eq!(registry.callers_of().values().next().unwrap().len(), 2);

        on_evicted(&cache, &registry, &keys[0]);
        assert_eq!(
            registry.callers_of().values().next().unwrap(),
            &[keys[1].clone()]
        );
        on_evicted(&cache, &registry, &keys[1]);
        assert!(registry.callers_of().is_empty());
    }

    fn source(name: &str, poll: &str, data: u32) -> String {
        format!(
            "[daemon.providers.p.sources.{name}]\n\
             data = \"{{{{ {data} }}}}\"\n\
             result.f{name} = \"{{{{ data }}}}\"\n\
             return = \"{{{{ result | poll('{poll}') }}}}\"\n"
        )
    }

    /// What a reload does to each instance: the Source that is gone or whose
    /// body changed is evicted, the one whose poll changed is retuned in place
    /// (and its stored schedule differs), the one left alone is untouched, and
    /// a disabled provider's instances all go.
    #[test]
    fn a_reload_plan_evicts_retunes_and_keeps() {
        let cache = Arc::new(Cache::new());
        let load = |toml: String| {
            ProviderRegistry::with_config(
                &crate::config::Config::from_toml(&toml),
                &Default::default(),
            )
            .unwrap()
            .share(&cache)
        };
        let before = load(format!(
            "{}{}{}{}",
            source("keep", "5s", 1),
            source("tune", "5s", 1),
            source("gone", "5s", 1),
            source("body", "5s", 1)
        ));
        let mut lifecycle = LifecycleRegistry::new();
        let key = |source: &str| lifecycle::Key {
            provider: "p".into(),
            path: None,
            source: source.into(),
            node: String::new(),
        };
        for name in ["keep", "tune", "gone", "body"] {
            let sm = before.source_metadata("p", name).unwrap();
            let config = source_lifecycle_config(&cache, "p", None, sm);
            lifecycle.on_demand(key(name), config, Instant::now());
        }
        let put = lifecycle::Key {
            source: crate::cache::PUT_SOURCE.into(),
            node: "n".into(),
            ..key("")
        };
        apply_put_written(&mut lifecycle, "p", None, "n", Some(60), Instant::now());

        let after = load(format!(
            "{}{}{}",
            source("keep", "5s", 1),
            source("tune", "9s", 1),
            source("body", "5s", 2)
        ));
        let plan = plan_reload(&cache, &lifecycle, &before, &after);
        let mut evicted = plan.evict.clone();
        evicted.sort_by(|a, b| a.source.cmp(&b.source));
        assert_eq!(evicted, vec![key("body"), key("gone")]);
        assert_eq!(plan.retune.len(), 1);
        let tune = &plan.retune[0];
        assert_eq!(tune.key, key("tune"));
        assert!(tune.stored_changed && !tune.failback_changed && tune.watches_changed);
        assert_eq!(tune.config.poll_interval, Some(Duration::from_secs(9)));
        assert!(
            !plan.evict.contains(&put) && plan.retune.iter().all(|r| r.key != put),
            "a put is never touched"
        );

        let off = load(format!(
            "{}{}{}[daemon.providers.p]\nenabled = false\n",
            source("keep", "5s", 1),
            source("tune", "5s", 1),
            source("gone", "5s", 1)
        ));
        let mut evicted = plan_reload(&cache, &lifecycle, &before, &off).evict;
        evicted.sort_by(|a, b| a.source.cmp(&b.source));
        assert_eq!(
            evicted,
            vec![key("body"), key("gone"), key("keep"), key("tune")]
        );
    }

    /// A decay depth edit changes no Source, and retunes every instance of the
    /// provider in place without a restamp (the Source sets no decay of its own).
    #[test]
    fn a_decay_depth_edit_retunes_in_place() {
        let cache = Arc::new(Cache::new());
        let registry = ProviderRegistry::with_config(
            &crate::config::Config::from_toml(&source("a", "5s", 1)),
            &Default::default(),
        )
        .unwrap()
        .share(&cache);
        let mut lifecycle = LifecycleRegistry::new();
        let sm = registry.source_metadata("p", "a").unwrap();
        let key = lifecycle::Key {
            provider: "p".into(),
            path: None,
            source: "a".into(),
            node: String::new(),
        };
        lifecycle.on_demand(
            key.clone(),
            source_lifecycle_config(&cache, "p", None, sm),
            Instant::now(),
        );
        cache
            .write_static(
                "p:cache",
                None,
                "config:t",
                "_.decay",
                crate::provider::Value::Int(2),
                None,
            )
            .unwrap();
        let plan = plan_reload(&cache, &lifecycle, &registry, &registry);
        assert!(plan.evict.is_empty());
        assert_eq!(plan.retune.len(), 1);
        assert_eq!(plan.retune[0].config.decay, 2);
        assert!(!plan.retune[0].stored_changed && !plan.retune[0].watches_changed);
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
