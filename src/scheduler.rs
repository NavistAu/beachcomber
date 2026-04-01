use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::time::{Duration, interval};
use tracing::{debug, info, warn};

use crate::cache::Cache;
use crate::config::Config;
use crate::provider::InvalidationStrategy;
use crate::provider::registry::ProviderRegistry;
use crate::watcher::FsWatcher;

/// Messages sent from the Server to the Scheduler.
#[derive(Debug)]
pub enum SchedulerMessage {
    Poke {
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

/// Public wrapper for parse_duration_secs, used by script provider.
pub fn parse_duration_secs_pub(s: &str) -> Option<u64> {
    parse_duration_secs(s)
}

/// Parse a duration string like "30s", "5m", "1h" into seconds.
fn parse_duration_secs(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num_str, multiplier) = if let Some(stripped) = s.strip_suffix('s') {
        (stripped, 1u64)
    } else if let Some(stripped) = s.strip_suffix('m') {
        (stripped, 60)
    } else if let Some(stripped) = s.strip_suffix('h') {
        (stripped, 3600)
    } else {
        (s, 1)
    };
    num_str.trim().parse::<u64>().ok().map(|n| n * multiplier)
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

    pub fn poll_multiplier(&self) -> u64 {
        match self.stage {
            BackoffStage::Grace => 1,
            BackoffStage::SlowPoll => 4,
            BackoffStage::Frozen => 0,
            BackoffStage::Evict => 0,
        }
    }

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
}

impl FailureState {
    fn new() -> Self {
        Self {
            consecutive_failures: 0,
            suppressed_until: None,
        }
    }

    fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= 3 {
            // Exponential backoff: 1s, 2s, 4s, 8s, ... max 60s
            let delay_secs = (1u64 << (self.consecutive_failures - 3).min(6)).min(60);
            self.suppressed_until = Some(Instant::now() + Duration::from_secs(delay_secs));
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
    watch_paths: &HashMap<PathBuf, Vec<(String, Option<String>)>>,
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
    grace_duration: std::time::Duration,
) {
    backoff.entry(key.clone()).or_insert_with(|| {
        debug!("Starting backoff for provider={} path={:?}", key.0, key.1);
        BackoffState::new(grace_duration)
    });
}

type ProviderKeySet = Arc<std::sync::Mutex<std::collections::HashSet<(String, Option<String>)>>>;
type ProviderFailureMap = Arc<std::sync::Mutex<HashMap<(String, Option<String>), FailureState>>>;

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
}

impl Scheduler {
    pub fn new(
        cache: Arc<Cache>,
        registry: Arc<ProviderRegistry>,
        config: Config,
    ) -> (SchedulerHandle, Scheduler) {
        let (tx, rx) = mpsc::channel(256);
        let handle = SchedulerHandle::new(tx);
        let scheduler = Scheduler {
            cache,
            registry,
            config,
            rx,
            in_flight: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            pending_rerun: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            failure_counts: Arc::new(std::sync::Mutex::new(HashMap::new())),
        };
        (handle, scheduler)
    }

    /// Execute a provider on the blocking thread pool and write result to cache.
    /// This is fire-and-forget: returns immediately while the provider runs in the background.
    /// Deduplicates concurrent executions: if a provider is already running, marks it for
    /// a single rerun after completion rather than launching another concurrent execution.
    /// Suppresses execution when failure backoff is active.
    fn execute_provider(&self, provider_name: &str, path: Option<&str>) {
        let Some(provider) = self.registry.get(provider_name) else {
            warn!("Poke for unknown provider '{}'", provider_name);
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

        tokio::spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(timeout_secs),
                tokio::task::spawn_blocking(move || provider.execute(path_owned.as_deref())),
            )
            .await;

            // Record success or failure for backoff tracking.
            match &result {
                Ok(Ok(Some(_))) => {
                    failure_counts
                        .lock()
                        .unwrap()
                        .entry(key_for_cleanup.clone())
                        .or_insert_with(FailureState::new)
                        .record_success();
                }
                _ => {
                    failure_counts
                        .lock()
                        .unwrap()
                        .entry(key_for_cleanup.clone())
                        .or_insert_with(FailureState::new)
                        .record_failure();
                }
            }

            match result {
                Ok(Ok(Some(provider_result))) => {
                    cache.put_with_interval(
                        &name_owned,
                        path_for_cache.as_deref(),
                        provider_result,
                        poll_interval_secs,
                    );
                    debug!(
                        "Executed provider '{}' path={:?}",
                        name_owned, path_for_cache
                    );
                }
                Ok(Ok(None)) => {
                    debug!(
                        "Provider '{}' returned None for path={:?}",
                        name_for_log, path_for_cache
                    );
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
                    // Mark as in-flight again for this rerun.
                    in_flight.lock().unwrap().insert(key_for_cleanup.clone());
                    tokio::spawn(async move {
                        let rerun_result = tokio::time::timeout(
                            Duration::from_secs(timeout_secs),
                            tokio::task::spawn_blocking(move || {
                                rerun_provider.execute(rerun_path.as_deref())
                            }),
                        )
                        .await;

                        // Record success or failure for the rerun.
                        match &rerun_result {
                            Ok(Ok(Some(_))) => {
                                failure_counts
                                    .lock()
                                    .unwrap()
                                    .entry(key_for_cleanup.clone())
                                    .or_insert_with(FailureState::new)
                                    .record_success();
                            }
                            _ => {
                                failure_counts
                                    .lock()
                                    .unwrap()
                                    .entry(key_for_cleanup.clone())
                                    .or_insert_with(FailureState::new)
                                    .record_failure();
                            }
                        }

                        match rerun_result {
                            Ok(Ok(Some(r))) => {
                                cache.put(&rerun_name, key_for_cleanup.1.as_deref(), r);
                                debug!("Rerun provider '{}' completed", rerun_name);
                            }
                            Ok(Ok(None)) => {
                                debug!("Rerun provider '{}' returned None", rerun_name);
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
        let grace_duration =
            std::time::Duration::from_secs(self.config.lifecycle.grace_period_secs);

        // Watch paths that are being monitored: path -> (provider, path_arg)
        let mut watch_paths: HashMap<PathBuf, Vec<(String, Option<String>)>> = HashMap::new();

        // Demand tracking: (provider, path) -> last query time.
        // Keys queried within the demand window are kept warm with default polling.
        let mut demand: HashMap<(String, Option<String>), Instant> = HashMap::new();
        let demand_window_secs: u64 = 120; // Keep warm for 2 minutes after last query

        // Idle shutdown tracking.
        let mut last_activity = Instant::now();
        let idle_shutdown_secs = self.config.lifecycle.idle_shutdown_secs;

        // Tick every second to check poll timers.
        let mut tick = interval(Duration::from_secs(1));

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
                        Some(SchedulerMessage::Poke { provider, path }) => {
                            debug!("Poke: provider={} path={:?}", provider, path);
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

                                    if poll_secs > 0 {
                                        poll_states.entry(key.clone()).or_insert(PollState {
                                            last_run: Instant::now(),
                                            interval_secs: poll_secs,
                                        });
                                        debug!("Demand: started polling provider={} path={:?} every {}s", provider, path, poll_secs);
                                    }

                                    // Set up filesystem watching for path-scoped providers.
                                    if !meta.global
                                        && let Some(ref path_str) = path {
                                        let watch_path = PathBuf::from(path_str);
                                        if let Err(e) = fs_watcher.watch(&watch_path) {
                                            warn!("Failed to watch {:?}: {}", watch_path, e);
                                        } else {
                                            watch_paths
                                                .entry(watch_path)
                                                .or_default()
                                                .push((provider.clone(), path.clone()));
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

                // Poll tick — check which subscriptions are due.
                _ = tick.tick() => {
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
                        poll_states.remove(&key);

                        // Remove watch for this key.
                        let mut paths_to_unwatch = Vec::new();
                        for (watch_path, subscriptions) in watch_paths.iter_mut() {
                            subscriptions.retain(|(p, pa)| !(p == &key.0 && pa == &key.1));
                            if subscriptions.is_empty() {
                                paths_to_unwatch.push(watch_path.clone());
                            }
                        }
                        for wp in paths_to_unwatch {
                            watch_paths.remove(&wp);
                            let _ = fs_watcher.unwatch(&wp);
                        }

                        // Start backoff for eviction.
                        start_backoff_for_key(&key, &mut backoff, grace_duration);
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
        let grace_duration =
            std::time::Duration::from_secs(self.config.lifecycle.grace_period_secs);

        // Demand tracking: (provider, path) -> last query time.
        let mut demand: HashMap<(String, Option<String>), Instant> = HashMap::new();
        let demand_window_secs: u64 = 120;

        let mut tick = interval(Duration::from_secs(1));

        // Idle shutdown tracking.
        let mut last_activity = Instant::now();
        let idle_shutdown_secs = self.config.lifecycle.idle_shutdown_secs;

        loop {
            tokio::select! {
                msg = self.rx.recv() => {
                    match msg {
                        None | Some(SchedulerMessage::Shutdown) => break,
                        Some(SchedulerMessage::Poke { provider, path }) => {
                            self.execute_provider(&provider, path.as_deref());
                            last_activity = Instant::now();
                        }
                        Some(SchedulerMessage::FsEvent { .. }) => {
                            // No-op without watcher.
                        }
                        Some(SchedulerMessage::GetStatus { reply }) => {
                            let empty_watch_paths: HashMap<PathBuf, Vec<(String, Option<String>)>> = HashMap::new();
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
                _ = tick.tick() => {
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
                        poll_states.remove(&key);
                        start_backoff_for_key(&key, &mut backoff, grace_duration);
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
                    match provider.execute(None) {
                        Some(result) => {
                            self.cache.put(&name, None, result);
                            info!("Computed initial value for provider '{}'", name);
                        }
                        None => {
                            warn!(
                                "Provider '{}' returned None during initial computation",
                                name
                            );
                        }
                    }
                }
            }
        }
    }

    fn handle_fs_event(
        &self,
        paths: Vec<PathBuf>,
        watch_paths: &HashMap<PathBuf, Vec<(String, Option<String>)>>,
    ) {
        for changed_path in &paths {
            // Find all subscriptions whose watch path is a prefix of the changed path.
            for (watch_path, subscriptions) in watch_paths {
                if changed_path.starts_with(watch_path) || changed_path == watch_path {
                    for (provider, path) in subscriptions {
                        debug!(
                            "FS event: re-executing provider={} path={:?}",
                            provider, path
                        );
                        self.execute_provider(provider, path.as_deref());
                    }
                }
            }
        }
    }
}
