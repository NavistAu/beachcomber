use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};
use tracing::{debug, info, warn};

use crate::cache::Cache;
use crate::config::Config;
use crate::provider::registry::ProviderRegistry;
use crate::provider::InvalidationStrategy;
use crate::subscription::{BackoffStage, BackoffState, SubscriptionManager};
use crate::watcher::FsWatcher;

/// Messages sent from the Server to the Scheduler.
#[derive(Debug)]
pub enum SchedulerMessage {
    Subscribe {
        consumer_id: u64,
        provider: String,
        path: Option<String>,
        triggers: TriggerSet,
    },
    Unsubscribe {
        consumer_id: u64,
        provider: String,
        path: Option<String>,
    },
    ConsumerDisconnected {
        consumer_id: u64,
    },
    Poke {
        provider: String,
        path: Option<String>,
    },
    FsEvent {
        paths: Vec<PathBuf>,
    },
    Shutdown,
}

/// The set of triggers a consumer requests for a subscription.
#[derive(Debug, Clone)]
pub struct TriggerSet {
    pub watch: bool,
    pub poll_secs: Option<u64>,
}

impl TriggerSet {
    pub fn from_protocol(triggers: &crate::protocol::SubscribeTriggers) -> Self {
        let poll_secs = triggers.poll.as_ref().and_then(|s| parse_duration_secs(s));
        Self {
            watch: triggers.watch,
            poll_secs,
        }
    }
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
    let (num_str, multiplier) = if s.ends_with('s') {
        (&s[..s.len() - 1], 1u64)
    } else if s.ends_with('m') {
        (&s[..s.len() - 1], 60)
    } else if s.ends_with('h') {
        (&s[..s.len() - 1], 3600)
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
}

/// Tracks the last execution time for polling purposes.
struct PollState {
    last_run: Instant,
    interval_secs: u64,
}

/// Start backoff for any subscription keys that currently have zero subscribers.
/// Only starts backoff if not already in backoff for that key.
fn start_backoff_for_empty_keys(
    subs: &SubscriptionManager,
    backoff: &mut HashMap<(String, Option<String>), BackoffState>,
    grace_duration: std::time::Duration,
) {
    for key in subs.keys_with_no_subscribers() {
        backoff.entry(key.clone()).or_insert_with(|| {
            debug!("Starting backoff grace for provider={} path={:?}", key.0, key.1);
            BackoffState::new(grace_duration)
        });
    }
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
        debug!("Removing backoff entry (evict) for provider={} path={:?}", key.0, key.1);
        backoff.remove(key);
    }

    to_evict
}

/// The scheduler core loop: executes providers on demand and manages subscriptions.
pub struct Scheduler {
    cache: Arc<Cache>,
    registry: Arc<ProviderRegistry>,
    config: Config,
    rx: mpsc::Receiver<SchedulerMessage>,
    /// Tracks which (provider, path) combinations are currently executing.
    in_flight: Arc<std::sync::Mutex<std::collections::HashSet<(String, Option<String>)>>>,
    /// Tracks which (provider, path) need to re-run after current execution completes.
    pending_rerun: Arc<std::sync::Mutex<std::collections::HashSet<(String, Option<String>)>>>,
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
        };
        (handle, scheduler)
    }

    /// Execute a provider on the blocking thread pool and write result to cache.
    /// This is fire-and-forget: returns immediately while the provider runs in the background.
    /// Deduplicates concurrent executions: if a provider is already running, marks it for
    /// a single rerun after completion rather than launching another concurrent execution.
    fn execute_provider(&self, provider_name: &str, path: Option<&str>) {
        let Some(provider) = self.registry.get(provider_name) else {
            warn!("Poke for unknown provider '{}'", provider_name);
            return;
        };

        let key = (provider_name.to_string(), path.map(|s| s.to_string()));

        // Check if already in flight — if so, queue a rerun and return.
        {
            let mut in_flight = self.in_flight.lock().unwrap();
            if in_flight.contains(&key) {
                self.pending_rerun.lock().unwrap().insert(key);
                debug!("Provider '{}' already in flight, queued rerun", provider_name);
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

        let path_for_cache = path_owned.clone();
        let name_for_log = name_owned.clone();
        let key_for_cleanup = key.clone();

        tokio::spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(timeout_secs),
                tokio::task::spawn_blocking(move || {
                    provider.execute(path_owned.as_deref())
                }),
            ).await;

            match result {
                Ok(Ok(Some(provider_result))) => {
                    cache.put(&name_owned, path_for_cache.as_deref(), provider_result);
                    debug!("Executed provider '{}' path={:?}", name_owned, path_for_cache);
                }
                Ok(Ok(None)) => {
                    debug!("Provider '{}' returned None for path={:?}", name_for_log, path_for_cache);
                }
                Ok(Err(e)) => {
                    warn!("Provider '{}' panicked: {}", name_for_log, e);
                }
                Err(_) => {
                    warn!("Provider '{}' timed out after {}s", name_for_log, timeout_secs);
                }
            }

            // Clear in-flight and check for pending reruns.
            in_flight.lock().unwrap().remove(&key_for_cleanup);
            let should_rerun = pending_rerun.lock().unwrap().remove(&key_for_cleanup);

            if should_rerun {
                debug!("Re-running provider '{}' (was queued during previous execution)", key_for_cleanup.0);
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
                        ).await;

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
                                warn!("Rerun provider '{}' timed out after {}s", rerun_name, timeout_secs);
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
                warn!("Failed to create filesystem watcher: {}. Watch triggers disabled.", e);
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

        // Subscription manager tracks who wants what.
        let mut subs = SubscriptionManager::new();

        // Poll states: (provider, path) -> PollState
        let mut poll_states: HashMap<(String, Option<String>), PollState> = HashMap::new();

        // Backoff states for keys with no active subscribers.
        let mut backoff: HashMap<(String, Option<String>), BackoffState> = HashMap::new();
        let grace_duration = std::time::Duration::from_secs(self.config.lifecycle.grace_period_secs);

        // Watch paths that are being monitored: path -> (provider, path_arg)
        let mut watch_paths: HashMap<PathBuf, Vec<(String, Option<String>)>> = HashMap::new();

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
                        }
                        Some(SchedulerMessage::Subscribe { consumer_id, provider, path, triggers }) => {
                            debug!("Subscribe: consumer={} provider={} path={:?}", consumer_id, provider, path);

                            let was_empty = subs.subscriber_count(&provider, path.as_deref()) == 0;
                            subs.subscribe(consumer_id, &provider, path.as_deref(), triggers.clone());

                            // Cancel backoff if someone is subscribing again.
                            let key = (provider.clone(), path.clone());
                            if backoff.remove(&key).is_some() {
                                debug!("Cancelled backoff for provider={} path={:?} (new subscriber)", provider, path);
                            }

                            // If no one was subscribed before, set up the triggers.
                            if was_empty {
                                // Set up filesystem watch if requested.
                                if triggers.watch {
                                    if let Some(path_str) = &path {
                                        let watch_path = PathBuf::from(path_str);
                                        if let Err(e) = fs_watcher.watch(&watch_path) {
                                            warn!("Failed to watch {:?}: {}", watch_path, e);
                                        } else {
                                            watch_paths
                                                .entry(watch_path)
                                                .or_default()
                                                .push((provider.clone(), path.clone()));
                                        }
                                    } else {
                                        // For global providers, watch based on metadata patterns.
                                        if let Some(prov) = self.registry.get(&provider) {
                                            let meta = prov.metadata();
                                            let patterns = match &meta.invalidation {
                                                InvalidationStrategy::Watch { patterns, .. } => patterns.clone(),
                                                InvalidationStrategy::WatchAndPoll { patterns, .. } => patterns.clone(),
                                                _ => vec![],
                                            };
                                            for pattern in &patterns {
                                                let watch_path = PathBuf::from(pattern);
                                                if let Err(e) = fs_watcher.watch(&watch_path) {
                                                    warn!("Failed to watch {:?}: {}", watch_path, e);
                                                } else {
                                                    watch_paths
                                                        .entry(watch_path)
                                                        .or_default()
                                                        .push((provider.clone(), path.clone()));
                                                }
                                            }
                                        }
                                    }
                                }

                                // Set up poll timer if requested.
                                if let Some(poll_secs) = triggers.poll_secs {
                                    let key = (provider.clone(), path.clone());
                                    poll_states.insert(key, PollState {
                                        last_run: Instant::now(),
                                        interval_secs: poll_secs,
                                    });
                                }
                            } else {
                                // Update effective poll interval.
                                let effective = subs.effective_triggers(&provider, path.as_deref());
                                if let Some(effective) = effective {
                                    let key = (provider.clone(), path.clone());
                                    if let Some(poll_secs) = effective.poll_secs {
                                        poll_states.entry(key).and_modify(|s| {
                                            s.interval_secs = poll_secs;
                                        }).or_insert(PollState {
                                            last_run: Instant::now(),
                                            interval_secs: poll_secs,
                                        });
                                    }
                                }
                            }

                            // Execute immediately if not cached.
                            if self.cache.get(&provider, path.as_deref()).is_none() {
                                self.execute_provider(&provider, path.as_deref());
                            }
                        }
                        Some(SchedulerMessage::Unsubscribe { consumer_id, provider, path }) => {
                            debug!("Unsubscribe: consumer={} provider={} path={:?}", consumer_id, provider, path);
                            subs.unsubscribe(consumer_id, &provider, path.as_deref());
                            start_backoff_for_empty_keys(&subs, &mut backoff, grace_duration);
                            self.cleanup_unused_subscriptions(&mut subs, &mut poll_states, &mut watch_paths, &mut fs_watcher);
                        }
                        Some(SchedulerMessage::ConsumerDisconnected { consumer_id }) => {
                            debug!("ConsumerDisconnected: consumer={}", consumer_id);
                            subs.disconnect(consumer_id);
                            start_backoff_for_empty_keys(&subs, &mut backoff, grace_duration);
                            self.cleanup_unused_subscriptions(&mut subs, &mut poll_states, &mut watch_paths, &mut fs_watcher);
                        }
                        Some(SchedulerMessage::FsEvent { paths }) => {
                            self.handle_fs_event(paths, &watch_paths);
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

                    // Advance backoff states and evict cache entries when needed.
                    let keys_to_evict = check_backoff(&mut backoff);
                    for (provider, path) in keys_to_evict {
                        debug!("Evicting cache for provider={} path={:?} (backoff evict)", provider, path);
                        self.cache.remove(&provider, path.as_deref());
                    }
                }
            }
        }
    }

    /// Fallback run loop when FsWatcher creation fails — no watch support.
    async fn run_without_watcher(mut self, mut _dummy_rx: mpsc::Receiver<Vec<PathBuf>>) {
        self.compute_once_providers();

        let mut subs = SubscriptionManager::new();
        let mut poll_states: HashMap<(String, Option<String>), PollState> = HashMap::new();
        let mut tick = interval(Duration::from_secs(1));

        loop {
            tokio::select! {
                msg = self.rx.recv() => {
                    match msg {
                        None | Some(SchedulerMessage::Shutdown) => break,
                        Some(SchedulerMessage::Poke { provider, path }) => {
                            self.execute_provider(&provider, path.as_deref());
                        }
                        Some(SchedulerMessage::Subscribe { consumer_id, provider, path, triggers }) => {
                            subs.subscribe(consumer_id, &provider, path.as_deref(), triggers.clone());
                            if let Some(poll_secs) = triggers.poll_secs {
                                let key = (provider.clone(), path.clone());
                                poll_states.entry(key).or_insert(PollState {
                                    last_run: Instant::now(),
                                    interval_secs: poll_secs,
                                });
                            }
                            if self.cache.get(&provider, path.as_deref()).is_none() {
                                self.execute_provider(&provider, path.as_deref());
                            }
                        }
                        Some(SchedulerMessage::Unsubscribe { consumer_id, provider, path }) => {
                            subs.unsubscribe(consumer_id, &provider, path.as_deref());
                        }
                        Some(SchedulerMessage::ConsumerDisconnected { consumer_id }) => {
                            subs.disconnect(consumer_id);
                        }
                        Some(SchedulerMessage::FsEvent { .. }) => {
                            // No-op without watcher.
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
                            warn!("Provider '{}' returned None during initial computation", name);
                        }
                    }
                }
            }
        }
    }

    fn handle_fs_event(&self, paths: Vec<PathBuf>, watch_paths: &HashMap<PathBuf, Vec<(String, Option<String>)>>) {
        for changed_path in &paths {
            // Find all subscriptions whose watch path is a prefix of the changed path.
            for (watch_path, subscriptions) in watch_paths {
                if changed_path.starts_with(watch_path) || changed_path == watch_path {
                    for (provider, path) in subscriptions {
                        debug!("FS event: re-executing provider={} path={:?}", provider, path);
                        self.execute_provider(provider, path.as_deref());
                    }
                }
            }
        }
    }

    fn cleanup_unused_subscriptions(
        &self,
        subs: &mut SubscriptionManager,
        poll_states: &mut HashMap<(String, Option<String>), PollState>,
        watch_paths: &mut HashMap<PathBuf, Vec<(String, Option<String>)>>,
        fs_watcher: &mut FsWatcher,
    ) {
        for (provider, path) in subs.keys_with_no_subscribers() {
            let key = (provider.clone(), path.clone());
            poll_states.remove(&key);

            // Remove watch paths associated with this subscription.
            let mut paths_to_unwatch = Vec::new();
            for (watch_path, subscriptions) in watch_paths.iter_mut() {
                subscriptions.retain(|(p, pa)| !(p == &provider && pa == &path));
                if subscriptions.is_empty() {
                    paths_to_unwatch.push(watch_path.clone());
                }
            }
            for wp in paths_to_unwatch {
                watch_paths.remove(&wp);
                if let Err(e) = fs_watcher.unwatch(&wp) {
                    debug!("Failed to unwatch {:?}: {}", wp, e);
                }
            }

            subs.remove_key(&provider, path.as_deref());
        }
    }
}
