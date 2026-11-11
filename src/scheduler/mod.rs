pub mod lifecycle;

#[cfg(test)]
mod provider_source_tests;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::{mpsc, oneshot};
// The scheduler's one clock is the runtime's: its ticks, its lifecycle and its
// failure backoff read it, as the cache's ages do. In a daemon it is the
// system's monotonic clock; a test pauses and advances it.
use tokio::time::{Duration, Instant, interval, sleep_until};
use tracing::{debug, info, warn};

use crate::query::{Dir, Home, Instance};

pub use crate::cache::FailureSnapshot;
use crate::cache::{Cache, Origin};
use crate::config::Config;
use crate::provider::Schedule;
use crate::provider::registry::{ProviderRegistry, RegistryHandle};
use crate::run::{Overtake, Want};
use crate::scheduler::lifecycle::{
    LifecycleRegistry, LifecycleState, SourceLifecycleConfig, StateTransition, WatchAction,
};
use crate::watcher::{FsWatcher, Registrar};
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
    /// A `memo` wrote the memo of `provider`'s template leaf `node` at
    /// `path`. The address is a lifecycle entry of its own, started afresh
    /// and replacing every memo entry the write displaced there: a ttl is
    /// poll N x 1 with decay 1, and its `watch` paths are watched while it is
    /// Active. Its poll and its file events drop its hash; reads never renew
    /// it.
    MemoWritten {
        provider: String,
        path: Option<String>,
        node: String,
        ttl_secs: Option<u64>,
        watch: Vec<String>,
    },
    /// Request every instance's state, both halves taken at one moment: see
    /// [`snapshot_instances`].
    GetInstances {
        reply: tokio::sync::oneshot::Sender<HashMap<lifecycle::Key, InstanceSnapshot>>,
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
    /// `provider`'s `enabled.template` rendered `on` (`provider` is `p.s` for a
    /// Source's, which switches that Source alone). When the registry in
    /// force has it the other way, a copy with it switched is put in force
    /// as a reload would ([`ProviderRegistry::switched`]): only its Sources,
    /// and the Sources that call them, are touched.
    Switch {
        provider: String,
        on: bool,
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
    /// True unless the Source only polls ([`crate::provider::Schedule::watches_files`]).
    pub watches_files: bool,
    /// Source name for this lifecycle entry.
    pub source: String,
}

/// One instance's state at one moment: its lifecycle entry, when it has one,
/// and its failure streak, when it is failing. A failing instance may have no
/// lifecycle entry (it was evicted, or never demanded), and the reverse.
#[derive(Debug, Clone, Default)]
pub struct InstanceSnapshot {
    pub lifecycle: Option<LifecycleSnapshot>,
    pub failure: Option<FailureSnapshot>,
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
    /// Every path watched for a Source, by path, and where its registration
    /// stands.
    pub watches: Vec<crate::watcher::Watch>,
    pub in_flight: Vec<String>,
    pub lifecycle: Vec<LifecycleInfo>,
    pub poll_timers: Vec<PollTimerInfo>,
    pub demand: Vec<DemandInfo>,
    /// Provider file-watch backend chosen by the startup self-test:
    /// "native", "polling", or "disabled" (watcher creation failed).
    pub watch_backend: String,
}

// `DemandInfo`, `LifecycleInfo` and `PollTimerInfo` each name a lifecycle
// entry by its key: `source` is the Source's name, or `put` or `memo` for a
// static address, whose `node` is its address (omitted for a Source
// instance).

#[derive(Debug, Clone, serde::Serialize)]
pub struct DemandInfo {
    pub provider: String,
    pub path: Option<String>,
    pub source: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub node: String,
    pub last_query_secs_ago: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LifecycleInfo {
    pub provider: String,
    pub path: Option<String>,
    pub source: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub node: String,
    pub stage: String,
    pub elapsed_secs: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PollTimerInfo {
    pub provider: String,
    pub path: Option<String>,
    pub source: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub node: String,
    pub interval_secs: u64,
    pub last_run_secs_ago: u64,
}

/// What routes the events of one watched path to one lifecycle entry: the
/// entry's key, and the patterns an event below the path must match (none:
/// every event).
#[derive(Debug, Clone)]
pub(crate) struct Subscription {
    pub(crate) key: lifecycle::Key,
    pub(crate) patterns: Vec<String>,
}

/// How long a caller waits on the scheduler, for room in its queue and again
/// for an answer. What is asked of it is a hint or a report, and a request's
/// connection is a descriptor: a scheduler that has stopped draining its queue
/// costs a request this much, not its connection for good.
pub const SCHEDULER_WAIT: Duration = Duration::from_secs(1);

/// Handle for sending messages to the scheduler.
#[derive(Clone)]
pub struct SchedulerHandle {
    tx: mpsc::Sender<SchedulerMessage>,
    /// Whether the last send found no room in time: one stall, one log line.
    stalled: Arc<AtomicBool>,
}

impl SchedulerHandle {
    pub fn new(tx: mpsc::Sender<SchedulerMessage>) -> Self {
        Self {
            tx,
            stalled: Arc::default(),
        }
    }

    /// Queue `msg`, or drop it after [`SCHEDULER_WAIT`] with the queue still
    /// full. A demand lost this way comes again with the next read.
    pub async fn send(&self, msg: SchedulerMessage) {
        match self.tx.send_timeout(msg, SCHEDULER_WAIT).await {
            Err(mpsc::error::SendTimeoutError::Timeout(_)) => {
                if !self.stalled.swap(true, Ordering::Relaxed) {
                    warn!(
                        "the scheduler's queue has stayed full for {SCHEDULER_WAIT:?}: \
                         requests are answered from the cache without it"
                    );
                }
            }
            _ => self.stalled.store(false, Ordering::Relaxed),
        }
    }

    /// [`Self::send`] for a thread that is not on the runtime (the config
    /// watch's), waiting for room in the queue.
    pub fn send_blocking(&self, msg: SchedulerMessage) {
        let _ = self.tx.blocking_send(msg);
    }

    /// Send `msg` and wait for its `reply`: `None` when no answer comes in time.
    async fn ask<T>(&self, msg: SchedulerMessage, reply: oneshot::Receiver<T>) -> Option<T> {
        self.send(msg).await;
        tokio::time::timeout(SCHEDULER_WAIT, reply).await.ok()?.ok()
    }

    pub async fn get_status(&self) -> Option<SchedulerStatus> {
        let (reply, rx) = oneshot::channel();
        self.ask(SchedulerMessage::GetStatus { reply }, rx).await
    }

    /// Every instance with a lifecycle entry or a failure streak, each with
    /// both halves of its state taken at the same moment. Empty when the
    /// scheduler does not answer.
    pub async fn get_instances(&self) -> HashMap<lifecycle::Key, InstanceSnapshot> {
        let (reply, rx) = oneshot::channel();
        self.ask(SchedulerMessage::GetInstances { reply }, rx)
            .await
            .unwrap_or_default()
    }

    /// The failing instances: each with a non-zero streak.
    pub async fn get_failure_states(&self) -> HashMap<lifecycle::Key, FailureSnapshot> {
        let instances = self.get_instances().await;
        instances
            .into_iter()
            .filter_map(|(key, i)| Some((key, i.failure?)))
            .collect()
    }

    /// The instances with a lifecycle entry. A static entry (a put's, a
    /// memo's) has one only while it has a ttl or a watch.
    pub async fn get_lifecycle_snapshots(&self) -> HashMap<lifecycle::Key, LifecycleSnapshot> {
        let instances = self.get_instances().await;
        instances
            .into_iter()
            .filter_map(|(key, i)| Some((key, i.lifecycle?)))
            .collect()
    }
}

/// Tracks consecutive failures and suppression state for a (provider, path, source) key.
pub(crate) struct FailureState {
    consecutive_failures: u32,
    suppressed_until: Option<Instant>,
    threshold: u32,
    backoff_interval: Duration,
    /// The latest failure's message, whole: the `comb status` table cuts it
    /// for display, everything else carries all of it.
    last_error: Option<String>,
    /// A `log.fatal` stopped the instance: no run until something re-arms it
    /// ([`Self::rearm`], [`Self::holds_back`]). Never expires.
    stopped: bool,
    /// What the failed run read, to see whether any of it has changed since.
    inputs: Option<crate::cache::Validity>,
}

impl FailureState {
    pub(crate) fn new(threshold: u32, backoff_interval: Duration) -> Self {
        Self {
            consecutive_failures: 0,
            suppressed_until: None,
            threshold,
            backoff_interval,
            last_error: None,
            stopped: false,
            inputs: None,
        }
    }

    /// A run failed with `log.fatal`: stopped, with the inputs it read.
    pub(crate) fn record_fatal(&mut self, message: &str, inputs: Option<crate::cache::Validity>) {
        self.record_failure(message);
        self.stopped = true;
        self.suppressed_until = None;
        self.inputs = inputs;
    }

    /// The next demand runs it: the streak, any backoff and any stop end.
    pub(crate) fn rearm(&mut self) {
        self.record_success();
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped
    }

    /// Whether a run is held back now. A stopped instance whose failed run's
    /// inputs have changed (one `stat` each) is re-armed, and is not.
    pub(crate) fn holds_back(&mut self) -> bool {
        use libbeachcomber::validity::Check;
        if self.stopped
            && self
                .inputs
                .as_ref()
                .is_some_and(|v| v.check() == Check::Invalid)
        {
            self.rearm();
        }
        self.stopped || self.is_suppressed()
    }

    pub(crate) fn record_failure(&mut self, message: &str) {
        self.stopped = false;
        self.inputs = None;
        self.consecutive_failures += 1;
        self.last_error = Some(message.to_string());
        if self.consecutive_failures >= self.threshold {
            // 4 levels of exponential backoff from base interval, stays at level 4
            let level = (self.consecutive_failures - self.threshold).min(3);
            let delay = self.backoff_interval * (1u32 << level);
            self.suppressed_until = Some(Instant::now() + delay);
        }
    }

    pub(crate) fn record_success(&mut self) {
        self.stopped = false;
        self.inputs = None;
        self.consecutive_failures = 0;
        self.last_error = None;
        self.suppressed_until = None;
    }

    pub(crate) fn is_suppressed(&self) -> bool {
        self.suppressed_until
            .map(|until| Instant::now() < until)
            .unwrap_or(false)
    }

    /// How long the failback backoff still holds runs back, while it does.
    pub(crate) fn backoff_left(&self) -> Option<(Duration, u32)> {
        let left = self
            .suppressed_until?
            .saturating_duration_since(Instant::now());
        (!left.is_zero()).then_some((left, self.consecutive_failures))
    }

    /// The streak as `status` shows it; `None` once a success ended it.
    pub(crate) fn snapshot(&self) -> Option<FailureSnapshot> {
        (self.consecutive_failures > 0).then(|| FailureSnapshot {
            consecutive_failures: self.consecutive_failures,
            suppressed_until_unix_ms: self.suppressed_until.map(instant_to_unix_ms),
            stopped: self.stopped,
            last_error: self.last_error.clone(),
        })
    }
}

/// Every instance's state at `now`: each lifecycle entry and each failure
/// streak, joined on their key. The failure map is locked once, while the
/// scheduler holds the lifecycle, so the two halves agree.
fn snapshot_instances(
    lifecycle: &LifecycleRegistry,
    failures: &SourceFailureMap,
    now: Instant,
) -> HashMap<lifecycle::Key, InstanceSnapshot> {
    let mut out: HashMap<lifecycle::Key, InstanceSnapshot> = lifecycle
        .iter()
        .map(|(key, entry)| {
            let lifecycle = Some(snapshot_entry(key, entry, now));
            (
                key.clone(),
                InstanceSnapshot {
                    lifecycle,
                    failure: None,
                },
            )
        })
        .collect();
    let failures = failures.lock().unwrap_or_else(|e| e.into_inner());
    for (key, state) in failures.iter() {
        if let Some(failure) = state.snapshot() {
            out.entry(key.clone()).or_default().failure = Some(failure);
        }
    }
    out
}

/// Build a `LifecycleSnapshot` for a given lifecycle entry at `now`.
fn snapshot_entry(
    key: &lifecycle::Key,
    entry: &lifecycle::LifecycleEntry,
    now: Instant,
) -> LifecycleSnapshot {
    let watches_files = entry.config.schedule.watches_files();
    let poll_interval_secs = entry
        .poll_timer
        .as_ref()
        .map(|pt| pt.interval.as_secs())
        .unwrap_or(0);

    let decay = lifecycle::to_decay_level(&entry.state);
    let step_duration_secs = match decay {
        0 => entry.config.lifespan(),
        step => entry.config.step_length(step),
    }
    .as_secs();
    let poll_count = entry.config.poll_count();

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
        watches_files,
        source: key.source.to_string(),
    }
}

/// Convert a future `Instant` deadline into a Unix-millisecond timestamp by
/// anchoring against the current monotonic and wall clocks. Past deadlines
/// resolve to "now" (saturating).
fn instant_to_unix_ms(deadline: Instant) -> u64 {
    let now_inst = Instant::now();
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
    registrar: Option<&Registrar>,
    runs: &crate::run::Runs,
    watch_backend: &str,
) -> SchedulerStatus {
    let watches = registrar.map(Registrar::watches).unwrap_or_default();

    let in_flight_keys: Vec<String> = runs
        .in_flight()
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
        let (provider, path, node) = (&key.provider, &key.path, &key.node);
        // A memo's label as every reader sees it; a Source by its own name.
        let source = match key.source {
            Origin::Memo => key.source.presented_label(),
            _ => key.source.to_string(),
        };
        // Poll timer info — only for entries with a poll path.
        if let Some(pt) = &entry.poll_timer {
            poll_timer_info.push(PollTimerInfo {
                provider: provider.clone(),
                path: path.clone(),
                source: source.clone(),
                node: node.clone(),
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
                    source,
                    node: node.clone(),
                    last_query_secs_ago: entry.decay_timer.last_demand.elapsed().as_secs(),
                });
            }
            LifecycleState::Decay(n) => {
                // Decaying entries go in the lifecycle list.
                let elapsed_secs = entry.decay_timer.last_demand.elapsed().as_secs();
                backoff_info.push(LifecycleInfo {
                    provider: provider.clone(),
                    path: path.clone(),
                    source,
                    node: node.clone(),
                    stage: format!("Decay{n}"),
                    elapsed_secs,
                });
            }
        }
    }

    SchedulerStatus {
        watches,
        in_flight: in_flight_keys,
        lifecycle: backoff_info,
        poll_timers: poll_timer_info,
        demand: demand_info,
        watch_backend: watch_backend.to_string(),
    }
}

/// Failure backoff map: per-(provider, path, source) failure state.
pub(crate) type SourceFailureMap = Arc<std::sync::Mutex<HashMap<lifecycle::Key, FailureState>>>;

/// Whether an event at `event_path` is in the tree a `.` among the watch
/// `patterns` of an instance rooted at `root` covers, and not below a name a
/// `!name` pattern leaves out.
pub(crate) fn event_in_tree(
    patterns: &[String],
    root: &std::path::Path,
    event_path: &std::path::Path,
) -> bool {
    if !patterns.iter().any(|p| p == ".") {
        return false;
    }
    let Ok(relative) = event_path.strip_prefix(root) else {
        return false;
    };
    !relative.as_os_str().is_empty()
        && !patterns
            .iter()
            .filter_map(|p| p.strip_prefix('!'))
            .any(|left_out| {
                relative
                    .components()
                    .any(|c| c.as_os_str().to_string_lossy() == left_out)
            })
}

/// Whether an event at `event_path` matches any of the watch `patterns` of an
/// instance rooted at `root`, as a path it names: [`event_in_tree`] says
/// whether it is in the tree.
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
        if crate::provider::is_tree_pattern(pattern) {
            false
        } else if pattern.contains('/') {
            relative.starts_with(pattern)
        } else {
            relative
                .components()
                .any(|c| c.as_os_str().to_string_lossy() == pattern)
        }
    })
}

/// How long a Source's watched files stay quiet before a file event runs it.
/// One command makes a burst of changes (`git commit`: the index, a ref, a log,
/// `HEAD`), and a run during the burst reads a state that is about to go. A read
/// straight after the command must not find its budget shared with it.
const QUIET: Duration = Duration::from_millis(30);

/// However busy the files stay, a run this soon after the first change: what
/// watches them hears of a long operation as it goes.
const MOST_QUIET: Duration = Duration::from_millis(500);

/// The instances whose watched files changed and have yet to go quiet: for
/// each, when the first change was heard, and when it runs.
#[derive(Default)]
struct Settling {
    keys: HashMap<lifecycle::Key, (Instant, Instant)>,
}

impl Settling {
    fn changed(&mut self, key: lifecycle::Key, now: Instant) {
        let (first, due) = self.keys.entry(key).or_insert((now, now));
        *due = (now + QUIET).min(*first + MOST_QUIET);
    }

    /// When the next one is due.
    fn next(&self) -> Option<Instant> {
        self.keys.values().map(|(_, due)| *due).min()
    }

    /// Those due by `now`, taken.
    fn due(&mut self, now: Instant) -> Vec<lifecycle::Key> {
        let due: Vec<lifecycle::Key> = self
            .keys
            .iter()
            .filter(|(_, (_, at))| *at <= now)
            .map(|(key, _)| key.clone())
            .collect();
        for key in &due {
            self.keys.remove(key);
        }
        due
    }
}

/// The scheduler core loop: executes providers on demand and manages subscriptions.
pub struct Scheduler {
    cache: Arc<Cache>,
    registry: Arc<ProviderRegistry>,
    config: Config,
    rx: mpsc::Receiver<SchedulerMessage>,
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
    /// A watcher and its events a test hands in ([`Self::use_watcher`]).
    given_watcher: Option<(FsWatcher, mpsc::Receiver<Vec<PathBuf>>)>,
    /// The kernel-native backend: how `run()` makes a watcher on it and
    /// tests its delivery. The real one unless a test hands in its own
    /// ([`Self::use_native`]).
    native: Native,
}

type Watcher = (FsWatcher, mpsc::Receiver<Vec<PathBuf>>);
type SelfTest = Arc<dyn Fn() -> std::pin::Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

struct Native {
    watcher: Box<dyn Fn() -> notify::Result<Watcher> + Send + Sync>,
    self_test: SelfTest,
}

impl Default for Native {
    fn default() -> Self {
        Self {
            watcher: Box::new(FsWatcher::new),
            self_test: Arc::new(|| {
                Box::pin(crate::watcher::self_test_native_backend(
                    crate::watcher::WATCH_SELF_TEST_TIMEOUT,
                ))
            }),
        }
    }
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
        let failure_counts = Arc::clone(&registry.failures);
        cache.trees_watchable();
        let scheduler = Scheduler {
            cache,
            registry,
            config,
            rx,
            failure_counts,
            heartbeat,
            watchers,
            run_watch_self_test: false,
            polling_watcher: None,
            given_watcher: None,
            native: Native::default(),
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

    /// Re-execute `instances` of `provider`: a run of each that begins after
    /// now.
    fn refresh_sources(&self, provider: &str, instances: &[Instance]) {
        for instance in instances {
            // A refresh re-arms an instance a `log.fatal` stopped.
            let key = crate::provider::source_call::instance_key(
                provider,
                &instance.source,
                instance.root.address(),
            );
            if let Some(state) = self.failure_counts.lock().unwrap().get_mut(&key)
                && state.is_stopped()
            {
                state.rearm();
            }
            self.execute_source(
                provider,
                &instance.source,
                instance.root.address(),
                Want::Fresh,
            );
        }
    }

    /// Start the run of one instance of a Source that `want` needs (`root` is
    /// its cache address, `None` for the instance at `~`), through the one run
    /// path ([`crate::run::Runs::start`]): it joins or queues behind a run
    /// already in flight, whoever started that, applies the failback backoff,
    /// and records itself. The scheduler does not wait for it.
    fn execute_source(
        &self,
        provider_name: &str,
        source_name: &str,
        root: Option<&str>,
        want: Want,
    ) {
        if self.registry.source(provider_name, source_name).is_none() {
            warn!(
                "Refresh for unknown source '{}.{}'",
                provider_name, source_name
            );
            return;
        }
        let key = crate::provider::source_call::instance_key(provider_name, source_name, root);
        self.registry
            .runs()
            .start(want, &self.registry, &self.cache, &key);
    }

    /// What `key`'s poll or file event does: a Source instance runs (as
    /// `want` asks), a memo's hash goes, so the next render of its leaf
    /// writes it afresh, and a put's poll is its expiry clock, with nothing
    /// to run.
    fn fire(&self, key: &lifecycle::Key, want: Want) {
        match &key.source {
            Origin::Memo => {
                self.cache
                    .drop_memo_hash(&key.provider, key.path.as_deref(), &key.node);
            }
            Origin::Source(source) => {
                self.execute_source(&key.provider, source, key.path.as_deref(), want)
            }
            Origin::Put | Origin::Config(_) | Origin::Computed(_) => {}
        }
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
    /// A run in flight of an instance whose Source is gone or changed is
    /// overtaken and writes nothing; one whose Source was rescheduled runs
    /// again under the new Source ([`crate::run::Runs::supersede`]). Puts are
    /// not Sources and are never touched.
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

        // The failure state of a Source that is gone or changed goes, and
        // that of one whose failback changed starts afresh: a fixed body is
        // not held back by the backoff the old one earned. First, so a run
        // overtaken below that settles meanwhile does not have its rerun
        // held back by it.
        {
            let mut failures = self.failure_counts.lock().unwrap();
            for retune in plan.retune.iter().filter(|r| r.failback_changed) {
                failures.remove(&retune.key);
            }
            // A reload re-arms every instance a `log.fatal` stopped.
            failures.retain(|_, state| !state.is_stopped());
            failures.retain(|key, _| {
                key.source.is_static()
                    || !plan
                        .affected
                        .contains(&(key.provider.clone(), key.source.name().to_string()))
            });
        }

        // Every run in flight of an instance whose Source is gone, changed or
        // rescheduled, whoever started it and whether or not the instance has
        // a lifecycle entry yet: it writes nothing, and a rescheduled one runs
        // again under the Source now in force. Marked under the run table's
        // lock, so a run settling concurrently either wrote before this (the
        // eviction and restamp below then see its record) or is overtaken.
        let runs = Arc::clone(self.registry.runs());
        for key in runs.in_flight() {
            let named = (key.provider.clone(), key.source.name().to_string());
            let gone = plan.evict.contains(&key)
                || plan.affected.contains(&named)
                || self
                    .registry
                    .source(&key.provider, key.source.name())
                    .is_none();
            let rescheduled = plan.retune.iter().any(|r| r.key == key)
                || previous
                    .source_metadata(&key.provider, key.source.name())
                    .zip(
                        self.registry
                            .source_metadata(&key.provider, key.source.name()),
                    )
                    .is_some_and(|(before, now)| !same_schedule(before, now));
            if gone {
                runs.supersede(&key, Overtake::Gone, &self.registry, &self.cache);
            } else if rescheduled {
                runs.supersede(&key, Overtake::Again, &self.registry, &self.cache);
                if let Some(sm) = self
                    .registry
                    .source_metadata(&key.provider, key.source.name())
                {
                    self.cache.restamp_schedule(
                        &key.provider,
                        key.path.as_deref(),
                        key.source.name(),
                        &sm.schedule,
                    );
                }
            }
        }

        for key in &plan.evict {
            debug!(
                "Reload: evicting provider={} path={:?} source={}",
                key.provider, key.path, key.source
            );
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
            lifecycle.retune(&retune.key, retune.config.clone());
            if retune.stored_changed
                && let Some(sm) = self
                    .registry
                    .source_metadata(&retune.key.provider, retune.key.source.name())
            {
                self.cache.restamp_schedule(
                    &retune.key.provider,
                    retune.key.path.as_deref(),
                    retune.key.source.name(),
                    &sm.schedule,
                );
            }
            if retune.watches_changed {
                watches.drop.push(retune.key.clone());
                if retune.rewatch {
                    watches.rewatch.push(retune.key.clone());
                }
            }
        }
        registry.replace(Arc::clone(&self.registry));
        watches
    }

    /// Drop and register the watches a reload left to the run loop.
    fn rewatch(
        &self,
        watches: ReloadWatches,
        watch_paths: &mut HashMap<PathBuf, Vec<Subscription>>,
        registrar: Option<&Registrar>,
    ) {
        for key in &watches.drop {
            drop_watches_for_key(key, watch_paths, registrar);
            self.cache
                .tree_unwatched(&key.provider, key.path.as_deref(), key.source.name());
        }
        for key in &watches.rewatch {
            let Some(sm) = self
                .registry
                .source_metadata(&key.provider, key.source.name())
            else {
                continue;
            };
            watch_instance(
                registrar,
                watch_paths,
                &key.provider,
                sm,
                key.path.as_deref(),
                self.registry.home(),
            );
            if crate::provider::watches_tree(&sm.schedule) {
                tree_if_registered(&self.cache, registrar, key);
            }
        }
    }

    /// Opt this scheduler into the startup watch self-test. The daemon path sets this; directly-constructed
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

    /// Test seam: serve provider file-watching from `watcher` (a backend the
    /// test controls, [`FsWatcher::with_backend`]), whose events are what the
    /// test sends on `events`'s sender. No self-test.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn use_watcher(&mut self, watcher: FsWatcher, events: mpsc::Receiver<Vec<PathBuf>>) {
        self.given_watcher = Some((watcher, events));
        self.run_watch_self_test = false;
    }

    /// Test seam: make the "native" watcher with `watcher` and test its
    /// delivery with `self_test`, startup self-test and re-probe alike.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn use_native(
        &mut self,
        watcher: impl Fn() -> notify::Result<Watcher> + Send + Sync + 'static,
        self_test: impl Fn() -> std::pin::Pin<Box<dyn Future<Output = bool> + Send>>
        + Send
        + Sync
        + 'static,
    ) {
        self.native = Native {
            watcher: Box::new(watcher),
            self_test: Arc::new(self_test),
        };
    }

    pub async fn run(mut self) {
        // Provider file-watching starts on the kernel-native backend
        // immediately; the watch self-test runs concurrently rather than gating the loop — kernel
        // event delivery can take hundreds of ms even when healthy, and the
        // scheduler must serve demand from its first moment. If the self-test
        // fails, the loop swaps to the polling backend and re-registers every
        // watch path (a stream can be created without error and deliver
        // nothing — sandboxed CI hosts, degraded fseventsd; chosen blind,
        // watch-invalidated entries would silently never invalidate).
        // A watcher a test handed in is not the kernel's: it reports as
        // polling, which is what the self-test would leave it as.
        let polled = self.polling_watcher.is_some() || self.given_watcher.is_some();
        let watcher = match (self.given_watcher.take(), self.polling_watcher) {
            (Some(given), _) => Ok(given),
            (None, Some(interval)) => FsWatcher::new_polling(interval),
            (None, None) => (self.native.watcher)(),
        };
        // The registrations are made by the registrar's worker, which owns
        // the watcher; the loop keeps the events. Without a watcher the loop
        // still serves demand, polls and reloads; nothing is watched, and
        // `_idle_events` keeps the event channel open and silent.
        let (mut registrar, mut fs_rx, _idle_events) = match watcher {
            Ok((watcher, rx)) => (
                Some(Registrar::start(
                    watcher,
                    crate::watcher::WATCH_RETRY_INTERVAL,
                    self.registry.home().clone(),
                )),
                rx,
                None,
            ),
            Err(e) => {
                warn!(
                    "Failed to create filesystem watcher: {}. Watch triggers disabled.",
                    e
                );
                let (tx, rx) = mpsc::channel::<Vec<PathBuf>>(1);
                (None, rx, Some(tx))
            }
        };
        // A barrier needs a watcher that delivers in order, at once: not a
        // poll, nor a watcher a test sends its own events through.
        let barrier = (!polled)
            .then(crate::watcher::Barrier::new)
            .and_then(|made| made.map_err(|e| warn!("no barrier for reads: {e}")).ok());
        let barrier = barrier.map(Arc::new);
        if let (Some(barrier), Some(registrar)) = (&barrier, &registrar) {
            registrar.watch(barrier.dir().to_path_buf());
            self.cache.set_barrier(Arc::clone(barrier));
        }
        let mut settled_rx = registrar.as_mut().and_then(Registrar::take_settled);
        let mut watch_backend = registrar.as_ref().map(|_| {
            if polled {
                crate::watcher::WatchBackend::Polling
            } else {
                crate::watcher::WatchBackend::Native
            }
        });

        let mut self_test_rx: Option<tokio::sync::oneshot::Receiver<bool>> =
            if self.run_watch_self_test && registrar.is_some() {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let self_test = (self.native.self_test)();
                tokio::spawn(async move {
                    let ok = self_test.await;
                    let _ = tx.send(ok);
                });
                Some(rx)
            } else {
                None
            };
        // Set while on the polling fallback: fires once a re-run self-test passes.
        let mut reprobe_rx: Option<tokio::sync::oneshot::Receiver<()>> = None;
        let self_test = Arc::clone(&self.native.self_test);
        let reprobe = move || {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let self_test = Arc::clone(&self_test);
            tokio::spawn(async move {
                crate::watcher::until_native_delivers(|| self_test()).await;
                let _ = tx.send(());
            });
            Some(rx)
        };

        // Lifecycle registry: replaces the three old maps (demand, backoff, poll_states).
        let mut lifecycle = LifecycleRegistry::new();

        // Watch paths that are being monitored: path -> subscriptions
        let mut watch_paths: HashMap<PathBuf, Vec<Subscription>> = HashMap::new();

        // Idle shutdown tracking.
        let mut last_activity = Instant::now();
        let idle_shutdown = self.config.daemon().idle_shutdown();

        // The instances whose watched files changed, until they go quiet.
        let mut settling = Settling::default();

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
                        Some(SchedulerMessage::MemoWritten { provider, path, node, ttl_secs, watch }) => {
                            let key = memo_key(&provider, path.as_deref(), &node);
                            let displaced = apply_memo_written(&mut lifecycle, &self.cache, self.registry.home(), &key, ttl_secs, &watch, Instant::now());
                            for gone in &displaced {
                                drop_watches_for_key(gone, &mut watch_paths, registrar.as_ref());
                            }
                            watch_key(registrar.as_ref(), &mut watch_paths, &key, memo_watches(&key, &watch));
                            last_activity = Instant::now();
                        }
                        Some(SchedulerMessage::Reload { next, registry }) => {
                            last_activity = Instant::now();
                            let watches = self.reload(next, &registry, &mut lifecycle);
                            self.rewatch(watches, &mut watch_paths, registrar.as_ref());
                        }
                        // Not a client's doing: no activity for the idle
                        // shutdown.
                        Some(SchedulerMessage::Switch { provider, on, registry }) => {
                            if let Some(next) = self.registry.switched(&provider, on, &self.cache) {
                                info!("{provider}: enabled.template rendered {on}; switching it {}", if on { "on" } else { "off" });
                                let watches = self.reload(next, &registry, &mut lifecycle);
                                self.rewatch(watches, &mut watch_paths, registrar.as_ref());
                            }
                        }
                        Some(SchedulerMessage::GetStatus { reply }) => {
                            let status = build_status(
                                &lifecycle,
                                registrar.as_ref(),
                                self.registry.runs(),
                                watch_backend.map_or("disabled", |b| b.as_str()),
                            );
                            let _ = reply.send(status);
                        }
                        Some(SchedulerMessage::GetInstances { reply }) => {
                            let snap = snapshot_instances(&lifecycle, &self.failure_counts, Instant::now());
                            let _ = reply.send(snap);
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
                                let root = instance.root.clone().into_address();
                                let key = lifecycle::Key {
                                    provider: provider.clone(),
                                    path: root.clone(),
                                    source: Origin::source(&sm.name),
                                    node: String::new(),
                                };
                                let outcome = lifecycle.demand(
                                    key.clone(),
                                    || source_lifecycle_config(&self.cache, provider, root.as_deref(), sm, self.registry.home()),
                                    now,
                                );

                                // The instance's schedule registers when it is
                                // first requested (and again when decay dropped
                                // its watches and a demand reinstates them),
                                // never at load. A registration that failed is
                                // asked for again by the next demand.
                                if matches!(
                                    outcome.watch_registration,
                                    WatchAction::Register | WatchAction::Reinstate
                                ) {
                                    watch_instance(
                                        registrar.as_ref(),
                                        &mut watch_paths,
                                        provider,
                                        sm,
                                        root.as_deref(),
                                        self.registry.home(),
                                    );
                                    if crate::provider::watches_tree(&sm.schedule) {
                                        tree_if_registered(&self.cache, registrar.as_ref(), &key);
                                    }
                                } else if let Some(registrar) = &registrar
                                    && registrar.has_failed()
                                {
                                    registrar.retry(paths_of(&key, &watch_paths));
                                }

                                if matches!(outcome.transition, StateTransition::NewlyActive) {
                                    // Cold → Active: execute to populate the cache,
                                    // at the instance the lifecycle key names.
                                    self.execute_source(provider, &sm.name, root.as_deref(), Want::Cold);
                                }
                            }
                            last_activity = Instant::now();
                        }
                    }
                }

                // Filesystem events from watcher. A path watched through a
                // directory above it may just have been made: it is looked
                // for again, and its own creation is routed like any change.
                Some(paths) = fs_rx.recv() => {
                    if let Some(registrar) = &registrar {
                        for path in &paths {
                            registrar.look_again(path);
                        }
                    }
                    // A change in a watched tree runs nothing: the next read
                    // finds the record invalid, and a build's output that no
                    // read asks about costs nothing.
                    for key in resolve_tree_keys(&paths, &watch_paths) {
                        self.cache.tree_changed(&key.provider, key.path.as_deref(), key.source.name());
                    }
                    if let Some(barrier) = &barrier {
                        barrier.hear(&paths);
                    }
                    let affected_keys = resolve_keys_from_paths(&paths, &watch_paths);
                    for key in affected_keys {
                        let outcome = lifecycle.on_fsevent(key.clone(), Instant::now());
                        if outcome.refresh {
                            if key.source.is_static() {
                                self.fire(&key, Want::Fresh);
                            } else {
                                settling.changed(key, Instant::now());
                            }
                        }
                    }
                    last_activity = Instant::now();
                }

                // A registration has been made: a tree it covers is watched
                // from here.
                Some(path) = async { settled_rx.as_mut().expect("guarded by is_some").recv().await },
                    if settled_rx.is_some() =>
                {
                    for sub in watch_paths.get(&path).into_iter().flatten() {
                        let (provider, root, source) = (&sub.key.provider, sub.key.path.as_deref(), sub.key.source.name());
                        if sub.patterns.iter().any(|p| p == ".") && !self.cache.tree_is_watched(provider, root, source) {
                            self.cache.tree_watched(provider, root, source);
                        }
                    }
                }

                // What changed has gone quiet. A run that began before the
                // change may have read the file before it: one that begins
                // after it.
                _ = sleep_until(settling.next().unwrap_or_else(Instant::now)), if settling.next().is_some() => {
                    for key in settling.due(Instant::now()) {
                        self.fire(&key, Want::Fresh);
                    }
                }

                // Watch self-test verdict.
                // Failure hands the registrar the polling backend, which it
                // registers every watch on again.
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
                            Ok((poll_watcher, poll_rx)) => {
                                if let Some(registrar) = &registrar {
                                    registrar.replace_backend(poll_watcher);
                                }
                                fs_rx = poll_rx;
                                if let Some(barrier) = &barrier {
                                    barrier.off();
                                }
                                watch_backend = Some(crate::watcher::WatchBackend::Polling);
                                reprobe_rx = reprobe();
                            }
                            Err(e) => {
                                warn!(
                                    "failed to create polling fallback watcher: {e}; staying on native (events may not deliver)"
                                );
                            }
                        }
                    }
                }

                // A re-run self-test passed: the registrar takes the native
                // backend back, as it took the polling one.
                res = async { reprobe_rx.as_mut().expect("guarded by is_some").await },
                    if reprobe_rx.is_some() =>
                {
                    reprobe_rx = None;
                    if res.is_ok() {
                        match (self.native.watcher)() {
                            Ok((native, native_rx)) => {
                                if let Some(registrar) = &registrar {
                                    registrar.replace_backend(native);
                                }
                                fs_rx = native_rx;
                                if let Some(barrier) = &barrier {
                                    barrier.on();
                                }
                                watch_backend = Some(crate::watcher::WatchBackend::Native);
                                info!("watch self-test: fs events delivered again; provider watching back on native");
                            }
                            Err(e) => {
                                warn!("failed to create native watcher: {e}; staying on polling");
                                reprobe_rx = reprobe();
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
                            "Poll tick: provider={} path={:?} source={} node={}",
                            key.provider, key.path, key.source, key.node
                        );
                        // A run in flight brings the instance up to date as
                        // well as a new one would.
                        self.fire(key, Want::Any);
                    }

                    for key in &actions.watch_drops {
                        drop_watches_for_key(key, &mut watch_paths, registrar.as_ref());
                        self.cache.tree_unwatched(&key.provider, key.path.as_deref(), key.source.name());
                    }

                    for key in &actions.evictions {
                        debug!(
                            "Evicting source provider={} path={:?} source={}",
                            key.provider, key.path, key.source
                        );
                        // Evict only this source's contribution; the entry is removed
                        // only when its last source evicts (lifecycle is keyed
                        // per (provider, path, source)).
                        on_evicted(&self.cache, &self.registry, key);
                        drop_watches_for_key(key, &mut watch_paths, registrar.as_ref());
                        self.cache.tree_forget(&key.provider, key.path.as_deref(), key.source.name());
                    }

                    for (key, new_state) in &actions.transitions {
                        debug!("lifecycle: key {:?} -> {:?}", key, new_state);
                    }

                    // Idle shutdown: the daemon's own config seeds do not
                    // count, anything a client or a run wrote does.
                    if let Some(idle) = idle_shutdown
                        && self.cache.holds_only_config_seeds()
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
        // The cache that holds it outlives this, and a daemon stops without
        // dropping it.
        if let Some(barrier) = &barrier {
            barrier.remove();
        }
    }
}

/// Subscribe one instance of `provider`'s Source `sm` at `root` to its watches
/// ([`instance_watches`]).
fn watch_instance(
    registrar: Option<&Registrar>,
    watch_paths: &mut HashMap<PathBuf, Vec<Subscription>>,
    provider: &str,
    sm: &crate::provider::SourceMetadata,
    root: Option<&str>,
    home: &Home,
) {
    let key = lifecycle::Key {
        provider: provider.to_string(),
        path: root.map(str::to_string),
        source: Origin::source(&sm.name),
        node: String::new(),
    };
    let watches = instance_watches(&key, sm, home);
    watch_key(registrar, watch_paths, &key, watches);
}

/// The watches an instance of a Source (`key`) registers when it is first
/// requested: each as the path to watch and the subscription that routes its
/// events to the instance's lifecycle key.
///
/// A relative `watch` entry is a pattern under the instance's root directory,
/// so it registers only for a rooted instance: at `~` a recursive watch of
/// `$HOME` is the firehose, and the instance refreshes by poll, by demand and
/// when a read finds an input changed. An absolute entry (`/…`, or `~`/`$XDG_*`
/// expanded by the Source) is watched whatever the root, and an event on it
/// refreshes this instance. A Source that only polls names no path and
/// registers no watch at all.
fn instance_watches(
    key: &lifecycle::Key,
    sm: &crate::provider::SourceMetadata,
    home: &Home,
) -> Vec<(PathBuf, Subscription)> {
    let mut out = Vec::new();
    let subscription = |patterns: Vec<String>| Subscription {
        key: key.clone(),
        patterns,
    };
    let patterns = crate::provider::watch_patterns(&sm.schedule);
    if let Some(root) = &key.path
        && !patterns.is_empty()
    {
        out.push((PathBuf::from(root), subscription(patterns.clone())));
    }
    // The instance at `~` has no directory to watch recursively: that would be
    // all of `$HOME`. Its files are the single-component patterns joined to
    // `$HOME` (`mise.toml`, `.tool-versions`), each watched on its own.
    if key.path.is_none()
        && let Some(home) = home.dir()
    {
        for pattern in &patterns {
            let file = Path::new(home).join(pattern);
            if !pattern.contains('/') && !crate::provider::is_tree_pattern(pattern) && file.exists()
            {
                out.push((file, subscription(Vec::new())));
            }
        }
    }
    for abs in &sm.schedule.watch_abs {
        out.push((PathBuf::from(abs), subscription(Vec::new())));
    }
    out
}

/// The watches of the memo `key`: each of its write-back's absolute paths,
/// every event on it or below it routed to the memo. A path that does not
/// exist yet is watched through the nearest directory above it that does,
/// and its creation is an event ([`Registrar`]).
fn memo_watches(key: &lifecycle::Key, watch: &[String]) -> Vec<(PathBuf, Subscription)> {
    watch
        .iter()
        .map(|path| {
            let sub = Subscription {
                key: key.clone(),
                patterns: Vec::new(),
            };
            (PathBuf::from(path), sub)
        })
        .collect()
}

/// Bring `key`'s subscriptions to `watches`: a path it is subscribed to and
/// no longer names lets go of it, one it names and is not subscribed to yet
/// registers ([`subscribe`]), and one it keeps keeps its registration. A
/// registration of one of its paths that failed is asked for again. With no
/// watcher there is nothing to watch with.
fn watch_key(
    registrar: Option<&Registrar>,
    watch_paths: &mut HashMap<PathBuf, Vec<Subscription>>,
    key: &lifecycle::Key,
    watches: Vec<(PathBuf, Subscription)>,
) {
    let Some(registrar) = registrar else {
        return;
    };
    let held: Vec<PathBuf> = paths_of(key, watch_paths).map(PathBuf::from).collect();
    for path in held
        .iter()
        .filter(|p| !watches.iter().any(|(w, _)| w == *p))
    {
        unsubscribe(key, path, watch_paths, Some(registrar));
    }
    for (path, sub) in watches {
        if !held.contains(&path) {
            debug!("Watching {path:?} for {key:?}");
            subscribe(registrar, watch_paths, path, sub);
        }
    }
    if registrar.has_failed() {
        registrar.retry(paths_of(key, watch_paths));
    }
}

/// Route `path`'s events to `sub`. The first subscriber of a path asks the
/// registrar for it, so Sources that watch one path (git's `refs` and
/// `status` at a repository's root) share one registration; `watch_paths` is
/// the one record of what is watched, which [`unsubscribe`] lets go of on the
/// last unsubscribe.
///
/// The registration is the registrar's worker's, so the loop goes on at once.
/// Until it is made the path is Pending and its events are not seen: the OS is
/// not watching it yet.
fn subscribe(
    registrar: &Registrar,
    watch_paths: &mut HashMap<PathBuf, Vec<Subscription>>,
    path: PathBuf,
    sub: Subscription,
) {
    match watch_paths.entry(path) {
        std::collections::hash_map::Entry::Occupied(mut subs) => subs.get_mut().push(sub),
        std::collections::hash_map::Entry::Vacant(first) => {
            registrar.watch(first.key().clone());
            first.insert(vec![sub]);
        }
    }
}

/// Stop routing `path`'s events to `key`, and stop watching the path when no
/// one else is subscribed to it.
fn unsubscribe(
    key: &lifecycle::Key,
    path: &std::path::Path,
    watch_paths: &mut HashMap<PathBuf, Vec<Subscription>>,
    registrar: Option<&Registrar>,
) {
    let Some(subs) = watch_paths.get_mut(path) else {
        return;
    };
    subs.retain(|sub| sub.key != *key);
    if subs.is_empty() {
        watch_paths.remove(path);
        if let Some(registrar) = registrar {
            registrar.unwatch(path);
        }
        debug!("Unwatched path {:?} (no subscriptions remaining)", path);
    }
}

/// The paths `key`'s entry is subscribed to.
fn paths_of<'a>(
    key: &'a lifecycle::Key,
    watch_paths: &'a HashMap<PathBuf, Vec<Subscription>>,
) -> impl Iterator<Item = &'a std::path::Path> {
    watch_paths
        .iter()
        .filter(|(_, subs)| subs.iter().any(|sub| sub.key == *key))
        .map(|(path, _)| path.as_path())
}

/// Apply a `PutWritten`: drop every `put` lifecycle key at or below `node`,
/// then, when the put has a ttl, make the address a lifecycle entry
/// ([`SourceLifecycleConfig::ttl`]): live for the ttl, stale for as long,
/// then evicted. Nothing renews it but the next put.
fn apply_put_written(
    lifecycle: &mut LifecycleRegistry,
    provider: &str,
    path: Option<&str>,
    node: &str,
    ttl_secs: Option<u64>,
    now: Instant,
) {
    lifecycle.remove_static_keys(&Origin::Put, provider, path, |address| {
        crate::cache::is_at_or_below(address, node)
    });
    let Some(ttl_secs) = ttl_secs else {
        return;
    };
    let key = lifecycle::Key {
        provider: provider.to_string(),
        path: path.map(str::to_string),
        source: Origin::Put,
        node: node.to_string(),
    };
    lifecycle.on_demand(key, SourceLifecycleConfig::ttl(ttl_secs), now);
}

/// The lifecycle key of the memo of `provider`'s template leaf `node` at
/// `path`.
fn memo_key(provider: &str, path: Option<&str>, node: &str) -> lifecycle::Key {
    lifecycle::Key {
        provider: provider.to_string(),
        path: path.map(str::to_string),
        source: Origin::Memo,
        node: node.to_string(),
    }
}

/// Apply a `MemoWritten` for the memo `key`: every memo entry at its
/// directory whose address overlaps it goes, as `Cache::write_memo` dropped
/// those memos, and the address starts its lifecycle afresh
/// ([`static_lifecycle_config`]), its clock and its poll from `now`. Returns
/// the entries displaced from other addresses, whose watches go.
fn apply_memo_written(
    lifecycle: &mut LifecycleRegistry,
    cache: &Cache,
    home: &Home,
    key: &lifecycle::Key,
    ttl_secs: Option<u64>,
    watch: &[String],
    now: Instant,
) -> Vec<lifecycle::Key> {
    let mut displaced = lifecycle.remove_static_keys(
        &Origin::Memo,
        &key.provider,
        key.path.as_deref(),
        |address| crate::cache::overlaps(address, &key.node),
    );
    displaced.retain(|gone| gone != key);
    let cfg = static_lifecycle_config(
        cache,
        home,
        &key.provider,
        key.path.as_deref(),
        ttl_secs,
        watch,
    );
    lifecycle.on_demand(key.clone(), cfg, now);
    displaced
}

/// The lifecycle of a memo's address. A ttl is a poll of count 0 with decay
/// 1, as a put's ([`SourceLifecycleConfig::ttl`]): valid for the ttl, then
/// stale for as long (a reader renders it again), then evicted, never
/// polled; `watch` paths beside it make it `watch_and_poll`. With none, it is watch-only, Active for the default
/// window and decaying at the depth `provider`'s `_` prototype gives at
/// `path` (`Cache::resolve_decay`).
fn static_lifecycle_config(
    cache: &Cache,
    home: &Home,
    provider: &str,
    path: Option<&str>,
    ttl_secs: Option<u64>,
    watch: &[String],
) -> SourceLifecycleConfig {
    let schedule = Schedule {
        poll: ttl_secs.map(|interval_secs| crate::provider::Poll {
            interval_secs,
            count: 0,
        }),
        ..Schedule::watch(Vec::new(), watch.to_vec())
    };
    let decay = if ttl_secs.is_some() {
        1
    } else {
        cache.resolve_decay(provider, &Dir::keyed(path), home)
    };
    SourceLifecycleConfig::new(schedule, decay)
}

/// The lifecycle a Source of `provider` runs under: its own strategy, with the
/// decay depth it sets itself, else the one its provider's `_` prototype gives
/// at `path` (`Cache::resolve_decay`).
fn source_lifecycle_config(
    cache: &Cache,
    provider: &str,
    path: Option<&str>,
    sm: &crate::provider::SourceMetadata,
    home: &Home,
) -> SourceLifecycleConfig {
    SourceLifecycleConfig::new(
        sm.schedule.clone(),
        sm.schedule
            .decay
            .unwrap_or_else(|| cache.resolve_decay(provider, &Dir::keyed(path), home)),
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
    /// Whether it should be watching now (it is Active) rather than waiting
    /// for a demand to reinstate it.
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
    alike(&a.schedule, &b.schedule)
}

/// Whether two schedules are alike in all but the `timeout`, which each run
/// reads from its Source as it starts: a reload that changes only that
/// leaves the instance, and a run in flight, as they are.
fn alike(a: &Schedule, b: &Schedule) -> bool {
    *a == Schedule {
        timeout: a.timeout,
        ..b.clone()
    }
}

/// Compare every Source instance in `lifecycle` as `previous` scheduled it with
/// what `next` says. Puts and memos are not Sources and are skipped: a memo
/// takes a new `_.decay` with its next write.
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
        if key.source.is_static() {
            continue;
        }
        let affected = plan
            .affected
            .contains(&(key.provider.clone(), key.source.name().to_string()));
        let Some(now) = next
            .source_metadata(&key.provider, key.source.name())
            .filter(|_| !affected)
        else {
            plan.evict.push(key.clone());
            continue;
        };
        let before = previous.source_metadata(&key.provider, key.source.name());
        let config =
            source_lifecycle_config(cache, &key.provider, key.path.as_deref(), now, next.home());
        if before.is_some_and(|before| same_schedule(before, now))
            && config.decay == entry.config.decay
            && alike(&config.schedule, &entry.config.schedule)
        {
            continue;
        }
        plan.retune.push(Retune {
            key: key.clone(),
            stored_changed: before.is_none_or(|before| {
                before.schedule.stored_keys(None) != now.schedule.stored_keys(None)
            }),
            failback_changed: before
                .is_none_or(|before| before.schedule.failback != now.schedule.failback),
            watches_changed: before
                .is_none_or(|before| before.schedule.invalidation() != now.schedule.invalidation()),
            rewatch: entry.state == LifecycleState::Active,
            config,
        });
    }
    plan
}

/// Evict one lifecycle key: a put address takes its own subtree with it, a
/// memo its value and its record, a Source its whole contribution.
fn evict(cache: &Cache, key: &lifecycle::Key) {
    let path = key.path.as_deref();
    match &key.source {
        Origin::Put => {
            cache.evict_put(&key.provider, path, &key.node);
            if let Some(store) = cache.put_store() {
                store.expire(&libbeachcomber::key::join(&key.provider, &key.node), path);
            }
        }
        Origin::Memo => {
            cache.evict_memo(&key.provider, path, &key.node);
        }
        Origin::Source(source) => cache.remove_source(&key.provider, path, source),
        Origin::Config(_) | Origin::Computed(_) => {}
    }
}

/// An instance left the lifecycle: take its records, the roots kept for it and
/// what it was known to call.
fn on_evicted(cache: &Cache, registry: &ProviderRegistry, key: &lifecycle::Key) {
    evict(cache, key);
    registry.logs.forget(key);
    registry.forget_roots(&key.provider, key.source.name(), key.path.as_deref());
    registry.forget_callees(&key.provider, key.source.name(), key.path.as_deref());
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
                    keys.push(sub.key.clone());
                }
            }
        }
    }
    keys
}

/// The instance `key`, just subscribed, has its tree watched already if its
/// root's registration has been made; otherwise when it is.
fn tree_if_registered(cache: &Cache, registrar: Option<&Registrar>, key: &lifecycle::Key) {
    if let (Some(registrar), Some(root)) = (registrar, &key.path)
        && registrar.is_active(std::path::Path::new(root))
    {
        cache.tree_watched(&key.provider, key.path.as_deref(), key.source.name());
    }
}

/// The instances whose watched tree `changed_paths` lie in.
fn resolve_tree_keys(
    changed_paths: &[PathBuf],
    watch_paths: &HashMap<PathBuf, Vec<Subscription>>,
) -> Vec<lifecycle::Key> {
    let mut keys = Vec::new();
    for changed_path in changed_paths {
        for (watch_path, subscriptions) in watch_paths {
            if !changed_path.starts_with(watch_path) {
                continue;
            }
            for sub in subscriptions {
                if event_in_tree(&sub.patterns, watch_path, changed_path) {
                    keys.push(sub.key.clone());
                }
            }
        }
    }
    keys
}

/// Remove every subscription of `key`, and unwatch each path no one else is
/// subscribed to.
fn drop_watches_for_key(
    key: &lifecycle::Key,
    watch_paths: &mut HashMap<PathBuf, Vec<Subscription>>,
    registrar: Option<&Registrar>,
) {
    let held: Vec<PathBuf> = paths_of(key, watch_paths).map(PathBuf::from).collect();
    for path in held {
        unsubscribe(key, &path, watch_paths, registrar);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn a_snapshot_counts_the_polls_that_fit_in_the_time_spent_in_the_step() {
        use lifecycle::*;
        let key = Key {
            provider: "git".into(),
            path: Some("/r".into()),
            source: crate::cache::Origin::source("main"),
            node: String::new(),
        };
        let config = SourceLifecycleConfig::new(Schedule::poll(60, 12), 1);
        let t0 = Instant::now();
        let mut registry = LifecycleRegistry::new();
        registry.on_demand(key.clone(), config, t0);
        let entry = registry.iter().next().unwrap().1;
        let at = |secs| snapshot_entry(&key, entry, t0 + Duration::from_secs(secs));
        assert_eq!(at(0).polls_elapsed, 0);
        assert_eq!(at(150).polls_elapsed, 2);
    }

    #[test]
    fn an_instance_snapshot_joins_its_lifecycle_and_its_failure() {
        use lifecycle::*;
        let key = |source: &str| Key {
            provider: "git".into(),
            path: Some("/r".into()),
            source: crate::cache::Origin::source(source),
            node: String::new(),
        };
        let now = Instant::now();
        let mut registry = LifecycleRegistry::new();
        registry.on_demand(key("live"), SourceLifecycleConfig::ttl(60), now);
        registry.on_demand(key("both"), SourceLifecycleConfig::ttl(60), now);
        let failures = SourceFailureMap::default();
        {
            let mut map = failures.lock().unwrap();
            for (source, fails) in [("both", true), ("failing", true), ("recovered", false)] {
                let state = map
                    .entry(key(source))
                    .or_insert_with(|| FailureState::new(3, Duration::from_secs(60)));
                state.record_failure("boom");
                if !fails {
                    state.record_success();
                }
            }
        }
        let snap = snapshot_instances(&registry, &failures, now);
        let has = |source: &str| {
            snap.get(&key(source))
                .map(|i| (i.lifecycle.is_some(), i.failure.is_some()))
        };
        assert_eq!(has("live"), Some((true, false)));
        assert_eq!(has("both"), Some((true, true)));
        assert_eq!(has("failing"), Some((false, true)));
        // A streak a success ended is not shown, as before.
        assert_eq!(has("recovered"), None);
    }

    #[test]
    fn a_fatal_failure_stops_with_no_expiry_until_rearmed_or_ended() {
        let mut state = FailureState::new(3, Duration::from_millis(1));
        state.record_fatal("boom", None);
        assert!(state.is_stopped() && state.holds_back());
        assert!(state.snapshot().unwrap().stopped);
        assert!(state.snapshot().unwrap().suppressed_until_unix_ms.is_none());
        // Inputs it read are unchanged, or it read none: it stays stopped.
        assert!(state.holds_back());
        state.rearm();
        assert!(!state.is_stopped() && !state.holds_back() && state.snapshot().is_none());
        // An error after a fatal, as a forced run's, is the ordinary kind.
        state.record_fatal("boom", None);
        state.record_failure("soft");
        assert!(!state.is_stopped());
    }

    #[test]
    fn a_failure_keeps_its_whole_message() {
        let mut state = FailureState::new(3, Duration::from_secs(60));
        state.record_failure("p.s: data: `false`: exit status: 1\nCaused by: more");
        assert_eq!(
            state.last_error.as_deref(),
            Some("p.s: data: `false`: exit status: 1\nCaused by: more")
        );
        let long = "x".repeat(500);
        state.record_failure(&long);
        assert_eq!(state.last_error.as_deref(), Some(long.as_str()));
        state.record_success();
        assert_eq!(state.last_error, None);
    }

    #[test]
    fn a_tree_event_is_anything_below_the_root_but_what_is_left_out() {
        let patterns: Vec<String> = [".git/index", ".", "!.git"].map(String::from).to_vec();
        let root = Path::new("/r");
        let in_tree = |p: &str| event_in_tree(&patterns, root, Path::new(p));
        assert!(in_tree("/r/README.md"));
        assert!(in_tree("/r/src/deep/new.txt"));
        assert!(in_tree("/r/.gitignore"));
        assert!(!in_tree("/r/.git/index"));
        assert!(!in_tree("/r/vendor/x/.git/HEAD"));
        assert!(!in_tree("/r"));
        assert!(!in_tree("/elsewhere/README.md"));
        assert!(!event_in_tree(
            &[".git/index".into()],
            root,
            Path::new("/r/README.md")
        ));
    }

    #[test]
    fn a_tree_pattern_names_no_path() {
        let patterns: Vec<String> = [".git/index", ".", "!.git"].map(String::from).to_vec();
        let root = Path::new("/r");
        assert!(event_matches_patterns(
            &patterns,
            root,
            Path::new("/r/.git/index")
        ));
        assert!(!event_matches_patterns(
            &patterns,
            root,
            Path::new("/r/README.md")
        ));
        assert!(!event_matches_patterns(
            &patterns,
            root,
            Path::new("/r/.git/x.lock")
        ));
    }

    fn put_key(path: Option<&str>, node: &str) -> lifecycle::Key {
        lifecycle::Key {
            provider: "build".into(),
            path: path.map(str::to_string),
            source: crate::cache::Origin::Put,
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
        let registry =
            crate::provider::registry::ProviderRegistry::shipped(&std::env::vars().collect());
        let mut sm = registry.provider_sources("git").unwrap()[0].clone();
        let cache = crate::cache::Cache::new();
        cache
            .write_static(
                "git:cache",
                None,
                &crate::cache::Origin::Put,
                "_.decay",
                crate::provider::Value::Int(6),
                None,
            )
            .unwrap();
        assert_eq!(sm.schedule.decay, None);
        assert_eq!(
            source_lifecycle_config(&cache, "git", None, &sm, &Home::default()).decay,
            6
        );
        sm.schedule.decay = Some(1);
        assert_eq!(
            source_lifecycle_config(&cache, "git", None, &sm, &Home::default()).decay,
            1
        );
    }

    #[test]
    fn a_source_registers_with_its_providers_decay() {
        let registry = ProviderRegistry::shipped(&std::env::vars().collect());
        let sm = registry.provider_sources("git").unwrap()[0].clone();
        let cache = Cache::new();
        assert_eq!(
            source_lifecycle_config(&cache, "git", None, &sm, &Home::default()).decay,
            lifecycle::DEFAULT_DECAY
        );
        cache
            .write_static(
                "git:cache",
                None,
                &crate::cache::Origin::Put,
                "_.decay",
                crate::provider::Value::Int(2),
                None,
            )
            .unwrap();
        assert_eq!(
            source_lifecycle_config(&cache, "git", None, &sm, &Home::default()).decay,
            2
        );
        assert_eq!(
            source_lifecycle_config(&cache, "mise", None, &sm, &Home::default()).decay,
            lifecycle::DEFAULT_DECAY,
            "another provider's prototype does not apply"
        );
        cache
            .write_static(
                "git:cache",
                Some("/w/proj"),
                &crate::cache::Origin::Put,
                "_.decay",
                crate::provider::Value::Int(6),
                None,
            )
            .unwrap();
        assert_eq!(
            source_lifecycle_config(&cache, "git", Some("/w/proj/sub"), &sm, &Home::default())
                .decay,
            6
        );
        assert_eq!(
            source_lifecycle_config(&cache, "git", Some("/w/other"), &sm, &Home::default()).decay,
            2
        );
    }

    /// Each put address is its own entry: valid for the ttl, stale for as
    /// long, then evicted, with no poll. Nothing but a put renews it.
    #[test]
    fn a_put_entry_is_not_renewed_by_reads() {
        let mut lifecycle = LifecycleRegistry::new();
        let t0 = Instant::now();
        apply_put_written(&mut lifecycle, "build", None, "status", Some(10), t0);

        // No read path touches a put key: the only API that renews an entry is
        // `on_demand`, and `apply_put_written` is its only put caller. The
        // ttl runs out regardless of what else happens.
        let actions = lifecycle.tick(t0 + Duration::from_secs(10));
        assert_eq!(
            actions.transitions.len(),
            1,
            "Active -> Decay(1) after the ttl"
        );
        assert!(actions.evictions.is_empty());
        assert!(actions.polls_due.is_empty());
        let actions = lifecycle.tick(t0 + Duration::from_secs(19));
        assert!(actions.evictions.is_empty() && actions.polls_due.is_empty());
        let actions = lifecycle.tick(t0 + Duration::from_secs(20));
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
        use crate::provider::Value;
        let cache = Cache::new();
        let write = |node: &str, v: i64| {
            cache
                .write_static(
                    "build",
                    None,
                    &crate::cache::Origin::Put,
                    node,
                    Value::Int(v),
                    Some(10),
                )
                .unwrap();
        };
        write("status", 1);
        write("eta", 2);

        evict(&cache, &put_key(None, "status"));

        let rows = cache.list_rows();
        let fields: Vec<&str> = rows.iter().map(|(_, r)| r.field.as_str()).collect();
        assert_eq!(fields, vec!["eta"]);
    }

    /// Each put address keeps the lifecycle its own put set: `build` expiring
    /// leaves `build.status`, which a deeper put owns.
    #[test]
    fn evicting_a_put_address_leaves_a_deeper_put_address() {
        use crate::provider::Value;
        use std::collections::HashMap;
        let cache = Cache::new();
        cache
            .write_static(
                "build",
                None,
                &crate::cache::Origin::Put,
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
                &crate::cache::Origin::Put,
                "status",
                Value::String("x".into()),
                Some(300),
            )
            .unwrap();

        evict(&cache, &put_key(None, ""));

        let rows = cache.list_rows();
        let fields: Vec<&str> = rows.iter().map(|(_, r)| r.field.as_str()).collect();
        assert_eq!(fields, vec!["status"]);
    }

    /// Sources that watch one path (git's `refs` and `status` at a repository's
    /// root) share one registration: the registrar is asked once, and both
    /// subscriptions route its events. The last to go lets the path go.
    #[test]
    fn subscribers_of_one_path_share_its_registration() {
        use crate::watcher::test_backend::{Call, Gate, Held};
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let (held, calls) = Held::new(&Gate::opened());
        let registrar = Registrar::start(
            FsWatcher::with_backend(Box::new(held)),
            crate::watcher::WATCH_RETRY_INTERVAL,
            Home::from_env(),
        );
        let mut watch_paths: HashMap<PathBuf, Vec<Subscription>> = HashMap::new();
        let key = |source: &str| lifecycle::Key {
            provider: "git".into(),
            path: Some(dir.display().to_string()),
            source: Origin::source(source),
            node: String::new(),
        };
        let sub = |source: &str, pattern: &str| Subscription {
            key: key(source),
            patterns: vec![pattern.to_string()],
        };
        subscribe(
            &registrar,
            &mut watch_paths,
            dir.clone(),
            sub("refs", ".git"),
        );
        subscribe(
            &registrar,
            &mut watch_paths,
            dir.clone(),
            sub("status", ".git/index"),
        );
        assert_eq!(watch_paths[&dir].len(), 2, "both subscriptions route");
        assert_eq!(registrar.watches().len(), 1, "one registration asked for");

        drop_watches_for_key(&key("refs"), &mut watch_paths, Some(&registrar));
        assert_eq!(registrar.watches().len(), 1, "status still watches it");
        drop_watches_for_key(&key("status"), &mut watch_paths, Some(&registrar));
        assert!(registrar.watches().is_empty() && watch_paths.is_empty());

        drop(registrar);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while calls.lock().unwrap().len() < 2 {
            assert!(std::time::Instant::now() < deadline, "{:?}", calls.lock());
            std::thread::yield_now();
        }
        assert_eq!(
            *calls.lock().unwrap(),
            [
                Call::Watch(dir.clone(), notify::RecursiveMode::Recursive),
                Call::Unwatch(dir.clone()),
            ]
        );
    }

    /// Config Sources `w<n>` with one instance, each watching its own
    /// directory under `base`, which exists.
    fn watching_sources(base: &Path, n: usize) -> (Config, Vec<PathBuf>) {
        let mut toml = String::new();
        let mut dirs = Vec::new();
        for i in 0..n {
            let dir = base.join(format!("w{i}"));
            std::fs::create_dir_all(&dir).unwrap();
            toml.push_str(&format!(
                "[daemon.providers.w{i}.sources.s]\n\
                 data = \"{{{{ 1 }}}}\"\n\
                 result.n = \"{{{{ data }}}}\"\n\
                 return = \"{{{{ result | watch('{}') }}}}\"\n",
                dir.display()
            ));
            dirs.push(dir);
        }
        (Config::from_toml(&toml), dirs)
    }

    fn demand(provider: &str) -> SchedulerMessage {
        SchedulerMessage::QueryActivity {
            provider: provider.into(),
            instances: vec![Instance::new("s", None)],
        }
    }

    fn states(status: &SchedulerStatus) -> Vec<(PathBuf, crate::watcher::WatchState)> {
        status
            .watches
            .iter()
            .map(|w| (w.path.clone(), w.state.clone()))
            .collect()
    }

    /// A registration the backend holds does not hold the loop: the heartbeat
    /// ticks, a second demand is taken while the first registration is still
    /// Pending, and both are Active once the backend lets them through.
    #[tokio::test(start_paused = true)]
    async fn the_loop_goes_on_while_a_registration_is_held() {
        use crate::watcher::WatchState::{Active, Pending};
        use crate::watcher::test_backend::{Gate, Held};
        let tmp = tempfile::tempdir().unwrap();
        let (config, dirs) = watching_sources(&tmp.path().canonicalize().unwrap(), 2);
        let cache = Arc::new(Cache::new());
        let registry = ProviderRegistry::with_config(&config, &std::env::vars().collect())
            .unwrap()
            .share(&cache);
        let (handle, mut scheduler) =
            Scheduler::new(cache, registry, config, Arc::new(WatcherRegistry::new()));
        let gate = Gate::closed();
        let (held, _) = Held::new(&gate);
        let (events, rx) = mpsc::channel(1);
        scheduler.use_watcher(FsWatcher::with_backend(Box::new(held)), rx);
        let heartbeat = scheduler.heartbeat();
        tokio::spawn(scheduler.run());

        handle.send(demand("w0")).await;
        let status = handle.get_status().await.expect("the loop answers");
        assert_eq!(states(&status), [(dirs[0].clone(), Pending)]);

        // The loop keeps draining file events meanwhile. A backend's callback
        // blocks on this channel when it is full, and FSEvents' `watch()` cannot
        // stop its stream while that callback runs: a loop that waited for the
        // registration deadlocked with it, for good.
        for _ in 0..3 {
            events.send(vec![dirs[0].join("x")]).await.unwrap();
        }

        handle.send(demand("w1")).await;
        let status = handle.get_status().await.expect("the loop answers");
        assert_eq!(
            states(&status),
            [(dirs[0].clone(), Pending), (dirs[1].clone(), Pending)],
            "the second demand was taken, its registration queued behind the first"
        );
        assert!(status.demand.iter().any(|d| d.provider == "w1"));

        // The clock is paused: the loop's ticks are the only thing that moves
        // the heartbeat, and the worker's real thread does not stop them.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let before = heartbeat.load(Ordering::Relaxed);
        tokio::time::advance(Duration::from_secs(3)).await;
        while heartbeat.load(Ordering::Relaxed) < before + 3 {
            assert!(
                std::time::Instant::now() < deadline,
                "the heartbeat stopped"
            );
            tokio::task::yield_now().await;
        }

        gate.open();
        let active = [
            (dirs[0].clone(), Active { via: None }),
            (dirs[1].clone(), Active { via: None }),
        ];
        while states(&handle.get_status().await.unwrap()) != active {
            assert!(std::time::Instant::now() < deadline, "never both Active");
            tokio::task::yield_now().await;
        }
    }

    /// A scheduler whose startup self-test fails serves its watches from
    /// polling; once a re-run of the self-test passes, after the backoff, it
    /// reports native again and has every watch registered on a new native
    /// watcher.
    #[tokio::test(start_paused = true)]
    async fn a_scheduler_on_polling_goes_back_to_native_when_the_self_test_passes() {
        use crate::watcher::WatchState::Active;
        use crate::watcher::test_backend::{Call, Gate, Held};
        use std::sync::Mutex;
        use std::sync::atomic::AtomicUsize;
        let tmp = tempfile::tempdir().unwrap();
        let (config, dirs) = watching_sources(&tmp.path().canonicalize().unwrap(), 1);
        let cache = Arc::new(Cache::new());
        let registry = ProviderRegistry::with_config(&config, &std::env::vars().collect())
            .unwrap()
            .share(&cache);
        let (handle, mut scheduler) =
            Scheduler::new(cache, registry, config, Arc::new(WatcherRegistry::new()));
        // Each "native" watcher made: what it was asked to watch, and the
        // sender that keeps its event channel open.
        type Made = (Arc<Mutex<Vec<Call>>>, mpsc::Sender<Vec<PathBuf>>);
        let made: Arc<Mutex<Vec<Made>>> = Arc::default();
        let tests = Arc::new(AtomicUsize::new(0));
        let (made_by, tested) = (Arc::clone(&made), Arc::clone(&tests));
        scheduler.self_test_watch_backend();
        scheduler.use_native(
            move || {
                let (held, calls) = Held::new(&Gate::opened());
                let (tx, rx) = mpsc::channel(1);
                made_by.lock().unwrap().push((calls, tx));
                Ok((FsWatcher::with_backend(Box::new(held)), rx))
            },
            // The startup self-test fails; every re-run passes.
            move || {
                let passes = tested.fetch_add(1, Ordering::SeqCst) > 0;
                Box::pin(async move { passes })
            },
        );
        tokio::spawn(scheduler.run());
        handle.send(demand("w0")).await;

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let watched = |status: &SchedulerStatus| {
            states(status).contains(&(dirs[0].clone(), Active { via: None }))
        };
        loop {
            let status = handle.get_status().await.expect("the loop answers");
            if status.watch_backend == "polling" && watched(&status) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "never on polling");
            tokio::task::yield_now().await;
        }
        assert_eq!(made.lock().unwrap().len(), 1, "no native watcher made yet");

        let (first, _) = crate::watcher::NATIVE_REPROBE_BACKOFF;
        tokio::time::advance(first).await;
        let rewatched = || {
            made.lock().unwrap().get(1).is_some_and(|(calls, _)| {
                calls
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|c| matches!(c, Call::Watch(p, _) if *p == dirs[0]))
            })
        };
        loop {
            let status = handle.get_status().await.expect("the loop answers");
            if status.watch_backend == "native" && watched(&status) && rewatched() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "never back on native with the watch registered on it"
            );
            tokio::task::yield_now().await;
        }
        assert_eq!(tests.load(Ordering::SeqCst), 2, "startup, then one re-run");
    }

    /// A scheduler that takes nothing from its queue and answers nothing costs
    /// each caller [`SCHEDULER_WAIT`] at most, and the answer it would have had
    /// is absent. Not the caller's connection: this wait, unbounded, is what let
    /// a wedged scheduler hold every request's descriptor until none were left.
    #[tokio::test(start_paused = true)]
    async fn a_scheduler_that_does_not_drain_its_queue_holds_nobody() {
        let (tx, _never_drained) = mpsc::channel(1);
        let handle = SchedulerHandle::new(tx);
        let refresh = || SchedulerMessage::Refresh {
            provider: "p".into(),
            instances: Vec::new(),
        };
        handle.send(refresh()).await;

        let began = tokio::time::Instant::now();
        handle.send(refresh()).await;
        assert_eq!(began.elapsed(), SCHEDULER_WAIT, "the send gave up");
        assert!(handle.get_status().await.is_none());
        assert!(handle.get_failure_states().await.is_empty());
        assert!(handle.get_lifecycle_snapshots().await.is_empty());
    }

    /// A message taken and never answered ends the wait too.
    #[tokio::test(start_paused = true)]
    async fn a_scheduler_that_takes_a_question_and_never_answers_holds_nobody() {
        let (tx, mut rx) = mpsc::channel(1);
        let handle = SchedulerHandle::new(tx);
        let asked = tokio::spawn(async move { handle.get_status().await });
        let _unanswered = rx.recv().await.unwrap();
        assert!(asked.await.unwrap().is_none());
    }

    /// T4.7, measured rather than asserted: the heartbeat's latest tick while
    /// a 20 000-file tree registers on the polling backend, whose first scan
    /// reads every file, and how long an `introspect`-style status request
    /// waits meanwhile. Ignored: it builds the tree and runs for seconds.
    ///
    /// `cargo nextest run --lib -E 'test(heartbeat_gap)' --run-ignored all --no-capture`
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn heartbeat_gap_during_a_slow_registration() {
        let tmp = tempfile::tempdir().unwrap();
        let tree = tmp.path().canonicalize().unwrap().join("tree");
        for d in 0..100 {
            let dir = tree.join(format!("d{d}"));
            std::fs::create_dir_all(&dir).unwrap();
            for f in 0..200 {
                std::fs::write(dir.join(format!("f{f}")), format!("{d}/{f}")).unwrap();
            }
        }
        let config = Config::from_toml(&format!(
            "[daemon.providers.big.sources.s]\n\
             data = \"{{{{ 1 }}}}\"\n\
             result.n = \"{{{{ data }}}}\"\n\
             return = \"{{{{ result | watch('{}') }}}}\"\n",
            tree.display()
        ));
        let cache = Arc::new(Cache::new());
        let registry = ProviderRegistry::with_config(&config, &std::env::vars().collect())
            .unwrap()
            .share(&cache);
        let (handle, mut scheduler) =
            Scheduler::new(cache, registry, config, Arc::new(WatcherRegistry::new()));
        // One scan: the registration's own.
        scheduler.use_polling_watcher(Duration::from_secs(3600));
        let heartbeat = scheduler.heartbeat();
        tokio::spawn(scheduler.run());
        let sampler = std::thread::spawn(move || {
            let mut ticks = Vec::new();
            let mut seen = heartbeat.load(Ordering::Relaxed);
            let end = Instant::now() + Duration::from_secs(8);
            while Instant::now() < end {
                let beat = heartbeat.load(Ordering::Relaxed);
                if beat != seen {
                    seen = beat;
                    ticks.push(Instant::now());
                }
                // No event announces a tick, so the counter is sampled.
                // sleep-justified: sampling a counter every millisecond
                std::thread::sleep(Duration::from_millis(1));
            }
            ticks
        });
        // Two ticks first, so the gaps measured are the ones the
        // registration falls in.
        // sleep-justified: let two ticks pass before the demand
        tokio::time::sleep(Duration::from_millis(2500)).await;
        let demanded = Instant::now();
        handle
            .send(SchedulerMessage::QueryActivity {
                provider: "big".into(),
                instances: vec![Instance::new("s", None)],
            })
            .await;
        handle.get_status().await.unwrap();
        let status_waited = demanded.elapsed();
        let ticks = sampler.join().unwrap();
        let late = ticks
            .windows(2)
            .map(|w| w[1] - w[0])
            .max()
            .unwrap()
            .saturating_sub(Duration::from_secs(1));
        eprintln!(
            "T4.7: {} ticks; the latest was {late:?} late; a status request sent with the \
             demand waited {status_waited:?}",
            ticks.len()
        );
    }

    /// A burst runs once, after the last of it; a burst that does not end runs
    /// at the cap, and starts over.
    #[test]
    fn a_change_waits_for_quiet_and_no_longer_than_the_cap() {
        let key = lifecycle::Key {
            provider: "p".into(),
            path: None,
            source: crate::cache::Origin::source("s"),
            node: String::new(),
        };
        let t0 = Instant::now();
        let mut settling = Settling::default();
        assert_eq!(settling.next(), None);
        settling.changed(key.clone(), t0);
        assert_eq!(settling.next(), Some(t0 + QUIET));
        let later = t0 + QUIET / 2;
        settling.changed(key.clone(), later);
        assert_eq!(
            settling.next(),
            Some(later + QUIET),
            "a change starts it over"
        );
        assert!(settling.due(later).is_empty());
        assert_eq!(settling.due(later + QUIET), vec![key.clone()]);
        assert_eq!(settling.next(), None, "taken");

        settling.changed(key.clone(), t0);
        settling.changed(key.clone(), t0 + MOST_QUIET - Duration::from_millis(1));
        assert_eq!(settling.next(), Some(t0 + MOST_QUIET), "the cap");
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
        let registry = ProviderRegistry::with_config(&config, &std::env::vars().collect())
            .unwrap()
            .share(&cache);
        let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a = registry.source("p", "a").unwrap();
        let mut keys = Vec::new();
        let rt = tokio::runtime::Runtime::new().unwrap();
        for dir in [&one, &two] {
            let at = dir.path().to_str().unwrap();
            // As the daemon runs it: on a blocking thread, where its call to
            // `b` starts and waits for `b`'s run.
            let (a, dir) = (Arc::clone(&a), at.to_string());
            rt.block_on(
                async move { tokio::task::spawn_blocking(move || a.execute(Some(&dir))).await },
            )
            .unwrap()
            .unwrap();
            keys.push(lifecycle::Key {
                provider: "p".into(),
                path: Dir::canonical(at, &Home::from_env()).into_address(),
                source: crate::cache::Origin::source("a"),
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

    /// The lines an instance's run raised go when it is evicted; another
    /// instance's stay.
    #[test]
    fn an_evicted_instance_forgets_its_lines() {
        let config = crate::config::Config::from_toml(
            "[daemon.providers.p.sources.a]\ndata = \"1\"\nresult.v = \"{{ data }}\"\n",
        );
        let cache = Arc::new(Cache::new());
        let registry = ProviderRegistry::with_config(&config, &std::env::vars().collect())
            .unwrap()
            .share(&cache);
        let key = |path: &str| lifecycle::Key {
            provider: "p".into(),
            path: Some(path.into()),
            source: crate::cache::Origin::source("a"),
            node: String::new(),
        };
        let line = || {
            vec![libbeachcomber::protocol::LogLine::new(
                libbeachcomber::protocol::Level::Warn,
                "source(a)",
                "careful",
            )]
        };
        registry.logs.store(&key("/one"), line());
        registry.logs.store(&key("/two"), line());
        on_evicted(&cache, &registry, &key("/one"));
        assert!(registry.logs.of(&key("/one")).is_empty());
        assert_eq!(registry.logs.of(&key("/two")), line());
    }

    fn source(name: &str, poll: &str, data: u32) -> String {
        format!(
            "[daemon.providers.p.sources.{name}]\n\
             data = \"{{{{ {data} }}}}\"\n\
             result.f{name} = \"{{{{ data }}}}\"\n\
             return = \"{{{{ result | poll('{poll}') }}}}\"\n"
        )
    }

    /// A reload that changes only a Source's `timeout` leaves its instance
    /// alone: each run reads the timeout as it starts.
    #[test]
    fn a_reload_of_only_the_timeout_retunes_nothing() {
        let cache = Arc::new(Cache::new());
        let load = |ret: &str| {
            let toml = format!(
                "[daemon.providers.p.sources.s]\ndata = \"{{{{ 1 }}}}\"\n\
                 result.f = \"{{{{ data }}}}\"\nreturn = \"{{{{ result | {ret} }}}}\"\n"
            );
            ProviderRegistry::with_config(
                &crate::config::Config::from_toml(&toml),
                &std::env::vars().collect(),
            )
            .unwrap()
            .share(&cache)
        };
        let before = load("poll('5s')");
        let after = load("poll('5s') | timeout('3s')");
        let (was, now) = (
            before.source_metadata("p", "s").unwrap(),
            after.source_metadata("p", "s").unwrap(),
        );
        assert_ne!(was.schedule.timeout, now.schedule.timeout);
        assert!(same_schedule(was, now));
        let mut lifecycle = LifecycleRegistry::new();
        let key = lifecycle::Key {
            provider: "p".into(),
            path: None,
            source: Origin::source("s"),
            node: String::new(),
        };
        let config = source_lifecycle_config(&cache, "p", None, was, &Home::default());
        lifecycle.on_demand(key, config, Instant::now());
        let plan = plan_reload(&cache, &lifecycle, &before, &after);
        assert!(plan.evict.is_empty() && plan.retune.is_empty());
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
                &std::env::vars().collect(),
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
            source: Origin::source(source),
            node: String::new(),
        };
        for name in ["keep", "tune", "gone", "body"] {
            let sm = before.source_metadata("p", name).unwrap();
            let config = source_lifecycle_config(&cache, "p", None, sm, &Home::default());
            lifecycle.on_demand(key(name), config, Instant::now());
        }
        let put = lifecycle::Key {
            source: crate::cache::Origin::Put,
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
        evicted.sort_by_key(|k| k.source.to_string());
        assert_eq!(evicted, vec![key("body"), key("gone")]);
        assert_eq!(plan.retune.len(), 1);
        let tune = &plan.retune[0];
        assert_eq!(tune.key, key("tune"));
        assert!(tune.stored_changed && !tune.failback_changed && tune.watches_changed);
        assert_eq!(tune.config.poll_interval(), Some(Duration::from_secs(9)));
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
        evicted.sort_by_key(|k| k.source.to_string());
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
            &std::env::vars().collect(),
        )
        .unwrap()
        .share(&cache);
        let mut lifecycle = LifecycleRegistry::new();
        let sm = registry.source_metadata("p", "a").unwrap();
        let key = lifecycle::Key {
            provider: "p".into(),
            path: None,
            source: crate::cache::Origin::source("a"),
            node: String::new(),
        };
        lifecycle.on_demand(
            key.clone(),
            source_lifecycle_config(&cache, "p", None, sm, &Home::default()),
            Instant::now(),
        );
        cache
            .write_static(
                "p:cache",
                None,
                &crate::cache::Origin::Config("t".into()),
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

    // ── memos ───────────────────────────────────────────────────────────────

    const H: &str = "9c6001d328a94690";

    /// Store the memo of `k.<node>` at `~` in `cache`, as the `memo` op does,
    /// and start its lifecycle at `at`, as its `MemoWritten` does.
    fn write_memo(
        cache: &Cache,
        lifecycle: &mut LifecycleRegistry,
        node: &str,
        ttl_secs: Option<u64>,
        watch: &[&str],
        at: Instant,
    ) -> lifecycle::Key {
        let watch: Vec<String> = watch.iter().map(|p| p.to_string()).collect();
        cache.write_memo(
            "k",
            None,
            node,
            crate::cache::MemoWrite {
                data: Some(crate::provider::Value::Int(1)),
                hash: H.into(),
                watch: watch.clone(),
                ttl_secs,
                log: Vec::new(),
            },
        );
        let key = memo_key("k", None, node);
        apply_memo_written(
            lifecycle,
            cache,
            &Home::default(),
            &key,
            ttl_secs,
            &watch,
            at,
        );
        key
    }

    /// A memo's ttl never polls it: past the ttl its record stays, hash and
    /// value, and reads stale, so a reader renders it again.
    #[test]
    fn a_memo_with_a_ttl_is_never_polled() {
        let cache = Cache::new();
        let mut lifecycle = LifecycleRegistry::new();
        let t0 = Instant::now();
        write_memo(&cache, &mut lifecycle, "f", Some(10), &[], t0);
        for s in [9, 10, 19, 20] {
            let actions = lifecycle.tick(t0 + Duration::from_secs(s));
            assert!(actions.polls_due.is_empty(), "at {s} s: {actions:?}");
        }
    }

    /// A ttl memo is valid for its ttl, then decays one step as long (its
    /// watches go as it does) and is evicted: its value and its record.
    #[test]
    fn a_memo_with_a_ttl_is_evicted_after_one_step() {
        let cache = Cache::new();
        let mut lifecycle = LifecycleRegistry::new();
        let t0 = Instant::now();
        let key = write_memo(&cache, &mut lifecycle, "f", Some(10), &["/w"], t0);
        let sibling = write_memo(&cache, &mut lifecycle, "g", None, &[], t0);

        let actions = lifecycle.tick(t0 + Duration::from_secs(10));
        assert_eq!(actions.watch_drops, vec![key.clone()]);
        assert_eq!(lifecycle.state(&key), Some(&LifecycleState::Decay(1)));
        assert!(
            lifecycle
                .tick(t0 + Duration::from_secs(19))
                .evictions
                .is_empty()
        );
        let actions = lifecycle.tick(t0 + Duration::from_secs(20));
        assert_eq!(actions.evictions, vec![key.clone()]);
        evict(&cache, &key);

        assert!(
            cache
                .memo_record(
                    "k",
                    &crate::query::Dir::Global,
                    &crate::query::Home::default(),
                    "f"
                )
                .is_none()
        );
        assert!(
            cache
                .stored_keys("k", None, &crate::cache::Origin::Memo, "f")
                .is_none()
        );
        assert!(
            cache
                .memo_record(
                    "k",
                    &crate::query::Dir::Global,
                    &crate::query::Home::default(),
                    "g"
                )
                .is_some(),
            "{sibling:?}"
        );
    }

    /// A memo's watch registers through the registrar (Pending, then Active),
    /// and an event on it drops the memo's hash.
    #[tokio::test]
    async fn a_memo_file_event_drops_the_hash() {
        use crate::watcher::WatchState::{Active, Pending};
        use crate::watcher::test_backend::{Gate, Held};
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().canonicalize().unwrap().join("config");
        std::fs::write(&file, "a").unwrap();
        let watchers = Arc::new(WatcherRegistry::new());
        let cache = Arc::new(Cache::with_watchers(watchers.clone()));
        let registry = ProviderRegistry::shipped(&std::env::vars().collect()).share(&cache);
        let (handle, mut scheduler) =
            Scheduler::new(cache.clone(), registry, Config::default(), watchers.clone());
        let gate = Gate::closed();
        let (held, _) = Held::new(&gate);
        let (events, rx) = mpsc::channel(1);
        scheduler.use_watcher(FsWatcher::with_backend(Box::new(held)), rx);
        tokio::spawn(scheduler.run());

        let watch = vec![file.display().to_string()];
        cache.write_memo(
            "k",
            None,
            "f",
            crate::cache::MemoWrite {
                data: Some(crate::provider::Value::Int(1)),
                hash: H.into(),
                watch: watch.clone(),
                ttl_secs: None,
                log: Vec::new(),
            },
        );
        handle
            .send(SchedulerMessage::MemoWritten {
                provider: "k".into(),
                path: None,
                node: "f".into(),
                ttl_secs: None,
                watch,
            })
            .await;
        let status = handle.get_status().await.expect("the loop answers");
        assert_eq!(states(&status), [(file.clone(), Pending)]);
        let named: Vec<(&str, &str)> = status
            .demand
            .iter()
            .map(|d| (d.source.as_str(), d.node.as_str()))
            .collect();
        assert_eq!(named, [("memo", "f")], "an Active entry, named by its node");

        gate.open();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while states(&handle.get_status().await.unwrap()) != [(file.clone(), Active { via: None })]
        {
            assert!(std::time::Instant::now() < deadline, "never Active");
            tokio::task::yield_now().await;
        }

        let mut woken = watchers.subscribe("k", None);
        events.send(vec![file.clone()]).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), woken.recv())
            .await
            .expect("the namespace is woken")
            .unwrap();
        let record = cache
            .memo_record(
                "k",
                &crate::query::Dir::Global,
                &crate::query::Home::default(),
                "f",
            )
            .unwrap();
        assert_eq!(record.hash(), None);
        assert_eq!(record.data, Some(crate::provider::Value::Int(1)));
    }

    /// A write-back starts its address's lifecycle again, its window from
    /// the write and its new ttl, and replaces the entries of the memos it
    /// displaced there (above or below it), which it returns.
    #[test]
    fn a_memo_write_replaces_its_entry() {
        let cache = Cache::new();
        let mut lifecycle = LifecycleRegistry::new();
        let t0 = Instant::now();
        let key = write_memo(&cache, &mut lifecycle, "f", Some(10), &[], t0);
        let t1 = t0 + Duration::from_secs(5);
        write_memo(&cache, &mut lifecycle, "f", Some(20), &[], t1);
        let entry = lifecycle
            .iter()
            .find(|(k, _)| **k == key)
            .map(|(_, e)| e.clone())
            .unwrap();
        assert!(entry.poll_timer.is_none(), "a ttl never polls");
        assert_eq!(
            (
                entry.decay_timer.last_demand,
                entry.decay_timer.step_deadline
            ),
            (t1, t1 + Duration::from_secs(20))
        );

        let below = write_memo(&cache, &mut lifecycle, "g.x", Some(10), &[], t0);
        let other = write_memo(&cache, &mut lifecycle, "h", Some(10), &[], t0);
        let group = memo_key("k", None, "g");
        let displaced = apply_memo_written(
            &mut lifecycle,
            &cache,
            &Home::default(),
            &group,
            None,
            &[],
            t1,
        );
        assert_eq!(displaced, vec![below]);
        let mut nodes: Vec<&str> = lifecycle.iter().map(|(k, _)| k.node.as_str()).collect();
        nodes.sort();
        assert_eq!(nodes, ["f", "g", "h"], "{other:?} stays");
    }

    /// Nothing but its write renews a memo's entry: no read demands it (a
    /// read's demand names Source instances), and a file event drops its hash
    /// without starting its window again. The ttl runs out regardless.
    #[test]
    fn a_memo_entry_is_not_renewed_by_reads() {
        let cache = Cache::new();
        let mut lifecycle = LifecycleRegistry::new();
        let t0 = Instant::now();
        let key = write_memo(&cache, &mut lifecycle, "f", Some(10), &["/w"], t0);

        let outcome = lifecycle.on_fsevent(key.clone(), t0 + Duration::from_secs(9));
        assert!(outcome.refresh && outcome.transition.is_none());
        let actions = lifecycle.tick(t0 + Duration::from_secs(10));
        assert_eq!(
            actions.transitions,
            vec![(key.clone(), LifecycleState::Decay(1))]
        );
        let actions = lifecycle.tick(t0 + Duration::from_secs(30));
        assert_eq!(actions.evictions, vec![key]);
    }

    /// Two memos watching one path share its registration, and an event on it
    /// is routed to each by its node; one letting go leaves the other.
    #[test]
    fn a_memo_key_routes_by_node() {
        use crate::watcher::test_backend::{Gate, Held};
        let (held, _) = Held::new(&Gate::opened());
        let registrar = Registrar::start(
            FsWatcher::with_backend(Box::new(held)),
            crate::watcher::WATCH_RETRY_INTERVAL,
            Home::from_env(),
        );
        let mut watch_paths: HashMap<PathBuf, Vec<Subscription>> = HashMap::new();
        let config = PathBuf::from("/h/.kube/config");
        let (f, g) = (memo_key("k", None, "f"), memo_key("k", None, "g"));
        for key in [&f, &g] {
            let watches = memo_watches(key, &["/h/.kube/config".to_string()]);
            watch_key(Some(&registrar), &mut watch_paths, key, watches);
        }
        assert_eq!(registrar.watches().len(), 1, "one registration");

        let routed = resolve_keys_from_paths(std::slice::from_ref(&config), &watch_paths);
        assert_eq!(routed, vec![f.clone(), g.clone()]);

        drop_watches_for_key(&f, &mut watch_paths, Some(&registrar));
        assert_eq!(
            resolve_keys_from_paths(&[config], &watch_paths),
            vec![g.clone()]
        );
        assert_eq!(registrar.watches().len(), 1, "g still watches it");
    }

    /// A write-back that names the paths its memo already watches keeps their
    /// registrations: only a path it no longer names is let go of, and only
    /// one it newly names is asked for.
    #[test]
    fn a_memo_rewrite_keeps_the_watches_it_still_names() {
        use crate::watcher::test_backend::{Call, Gate, Held};
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let [a, b, c] = ["a", "b", "c"].map(|n| {
            let path = dir.join(n);
            std::fs::write(&path, n).unwrap();
            path
        });
        let (held, calls) = Held::new(&Gate::opened());
        let registrar = Registrar::start(
            FsWatcher::with_backend(Box::new(held)),
            crate::watcher::WATCH_RETRY_INTERVAL,
            Home::from_env(),
        );
        let mut watch_paths: HashMap<PathBuf, Vec<Subscription>> = HashMap::new();
        let key = memo_key("k", None, "f");
        let paths = |ps: &[&PathBuf]| -> Vec<String> {
            ps.iter().map(|p| p.display().to_string()).collect()
        };
        for named in [paths(&[&a, &b]), paths(&[&b, &c])] {
            let watches = memo_watches(&key, &named);
            watch_key(Some(&registrar), &mut watch_paths, &key, watches);
        }
        let mut watched: Vec<&PathBuf> = watch_paths.keys().collect();
        watched.sort();
        assert_eq!(watched, [&b, &c]);

        drop(registrar);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while calls.lock().unwrap().len() < 4 {
            assert!(std::time::Instant::now() < deadline, "{:?}", calls.lock());
            std::thread::yield_now();
        }
        let recursive = notify::RecursiveMode::Recursive;
        assert_eq!(
            *calls.lock().unwrap(),
            [
                Call::Watch(a.clone(), recursive),
                Call::Watch(b.clone(), recursive),
                Call::Unwatch(a),
                Call::Watch(c, recursive),
            ],
            "b was registered once"
        );
    }

    /// A memo's ttl is a ttl as a put's is (never polled, one decay step as
    /// long), and watch paths beside it make it watch too; with no ttl a memo is watch-only, Active for the
    /// default window, at its provider's `_.decay`.
    #[test]
    fn a_memos_lifecycle_is_its_ttl_or_the_default_window() {
        let cache = Cache::new();
        let w = ["/w".to_string()];
        let cfg = static_lifecycle_config(&cache, &Home::default(), "k", None, Some(600), &[]);
        assert_eq!(cfg, SourceLifecycleConfig::ttl(600));
        assert_eq!(
            (cfg.lifespan(), cfg.decay, cfg.schedule.watches_files()),
            (Duration::from_secs(600), 1, false),
            "polled, watching nothing"
        );
        let cfg = static_lifecycle_config(&cache, &Home::default(), "k", None, Some(600), &w);
        assert_eq!(
            (cfg.poll_interval(), cfg.schedule.ttl()),
            (None, Some(Duration::from_secs(600)))
        );
        assert_eq!(
            (cfg.lifespan(), cfg.decay, cfg.schedule.watch_abs.clone()),
            (Duration::from_secs(600), 1, w.to_vec()),
            "a ttl that watches"
        );
        cache
            .write_static(
                "k:cache",
                None,
                &crate::cache::Origin::Put,
                "_.decay",
                crate::provider::Value::Int(2),
                None,
            )
            .unwrap();
        let cfg = static_lifecycle_config(&cache, &Home::default(), "k", None, None, &w);
        assert_eq!(
            (
                cfg.lifespan(),
                cfg.decay,
                cfg.schedule.poll,
                cfg.schedule.watches_files()
            ),
            (Duration::from_secs(720), 2, None, true),
            "watch-only"
        );
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
