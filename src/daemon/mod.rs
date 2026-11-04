pub mod config_watch;
pub mod defaults;
pub mod lifecycle;
pub mod seed;

use crate::cache::Cache;
use crate::config::Config;
use crate::provider::registry::{ProviderRegistry, RegistryHandle};
use crate::scheduler::{Scheduler, SchedulerMessage};
use crate::server::Server;
use crate::watcher_registry::WatcherRegistry;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// OS-bound: in-process daemon startup (used by tests and embedded mode)
// ---------------------------------------------------------------------------

/// Resolves once the daemon's socket is bound and connectable. If the daemon
/// refuses to start (config validation, bind failure) the sender is dropped and
/// awaiting this yields `Err`, so a caller never waits on a daemon that is not
/// coming.
pub type DaemonReady = tokio::sync::oneshot::Receiver<()>;

/// Start a daemon on the running runtime. Its environment is the process's
/// as it stands: the env file is not loaded here, since nothing may set the
/// environment once threads run. A caller that wants it calls
/// [`Config::load_env_file`] before starting its runtime, as `comb daemon` does.
pub fn start_in_process(
    socket_path: PathBuf,
    config: Config,
) -> (tokio::task::JoinHandle<()>, DaemonReady) {
    let cancel = CancellationToken::new();
    start_in_process_with_cancel(socket_path, config, cancel)
}

pub fn start_in_process_with_cancel(
    socket_path: PathBuf,
    config: Config,
    cancel: CancellationToken,
) -> (tokio::task::JoinHandle<()>, DaemonReady) {
    start_in_process_with_reaper(socket_path, config, cancel, None)
}

/// Daemon-path variant: also attaches reaper health for introspect surfacing
/// (canon singleton.md invariant 13).
pub fn start_in_process_with_reaper(
    socket_path: PathBuf,
    config: Config,
    cancel: CancellationToken,
    reaper: Option<std::sync::Arc<crate::singleton::ReaperHealth>>,
) -> (tokio::task::JoinHandle<()>, DaemonReady) {
    start_in_process_inner(socket_path, config, cancel, reaper, None, None)
}

/// How often a test daemon's file watching polls (the polling seam below):
/// a stat of each watched path, so a test sees a write within milliseconds.
#[cfg(any(test, feature = "test-helpers"))]
pub const TEST_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

/// Test seam: an in-process daemon whose file watching — its providers' and
/// its own configuration's — polls every [`TEST_POLL_INTERVAL`] instead of
/// registering kernel watches, with the watch self-test skipped. Kernel
/// FSEvents registration costs ~1.2s per `watch()` call on macOS — paid once
/// by the self-test and again per watched path — so a test that is not about
/// the native backend should not start one.
#[cfg(any(test, feature = "test-helpers"))]
pub fn start_in_process_polling(
    socket_path: PathBuf,
    config: Config,
) -> (tokio::task::JoinHandle<()>, DaemonReady) {
    start_in_process_polling_with(socket_path, config, CancellationToken::new(), None)
}

/// [`start_in_process_polling`] with a cancellation token and reaper health.
#[cfg(any(test, feature = "test-helpers"))]
pub fn start_in_process_polling_with(
    socket_path: PathBuf,
    config: Config,
    cancel: CancellationToken,
    reaper: Option<std::sync::Arc<crate::singleton::ReaperHealth>>,
) -> (tokio::task::JoinHandle<()>, DaemonReady) {
    start_in_process_inner(
        socket_path,
        config,
        cancel,
        reaper,
        Some(TEST_POLL_INTERVAL),
        None,
    )
}

/// Test seam: [`start_in_process_polling`] whose `introspect procs` samples
/// process starts through `procs` instead of the system's tracer, which
/// takes the whole sampling window and, unprivileged, sees nothing.
#[cfg(any(test, feature = "test-helpers"))]
pub fn start_in_process_polling_sampling(
    socket_path: PathBuf,
    config: Config,
    procs: Arc<dyn crate::boundaries::proc_snap::ProcessSnapshotter>,
) -> (tokio::task::JoinHandle<()>, DaemonReady) {
    start_in_process_inner(
        socket_path,
        config,
        CancellationToken::new(),
        None,
        Some(TEST_POLL_INTERVAL),
        Some(procs),
    )
}

fn start_in_process_inner(
    socket_path: PathBuf,
    config: Config,
    cancel: CancellationToken,
    reaper: Option<std::sync::Arc<crate::singleton::ReaperHealth>>,
    polling_watch: Option<std::time::Duration>,
    procs: Option<Arc<dyn crate::boundaries::proc_snap::ProcessSnapshotter>>,
) -> (tokio::task::JoinHandle<()>, DaemonReady) {
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        run_daemon_with_cancel(
            socket_path,
            config,
            cancel,
            reaper,
            ready_tx,
            polling_watch,
            procs,
        )
        .await;
    });
    (task, ready_rx)
}

async fn run_daemon_with_cancel(
    socket_path: PathBuf,
    config: Config,
    cancel: CancellationToken,
    reaper: Option<std::sync::Arc<crate::singleton::ReaperHealth>>,
    ready: tokio::sync::oneshot::Sender<()>,
    polling_watch: Option<std::time::Duration>,
    procs: Option<Arc<dyn crate::boundaries::proc_snap::ProcessSnapshotter>>,
) {
    let watchers = Arc::new(WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    // The daemon's own environment: what every config Source's lines see as
    // `env`, and what its commands inherit. Inside a running runtime nothing
    // may set it, so the env file is already in it: `comb daemon` loaded it
    // before its runtime started (an embedder does the same, or goes without).
    let vars: std::collections::HashMap<String, String> = std::env::vars().collect();
    let load = ProviderRegistry::load(&config, &vars);
    // Every error the configuration has, at once.
    if !load.errors.is_empty() {
        refuse_to_start(&load.errors);
        return;
    }
    for w in &load.warnings {
        warn!("{w}");
    }
    let registry = load.registry.share(&cache);

    let (handle, mut scheduler) = Scheduler::new(
        cache.clone(),
        registry.clone(),
        config.clone(),
        watchers.clone(),
    );

    // Facet tables seed their namespaces before the socket is served, so the
    // first `get` already sees them, and the same files are watched for changes:
    // an edit under `providers` reloads in place, a bare `[daemon]` or
    // `[client]` key cancels `cancel` and the daemon restarts. The seed is
    // the one the load planned.
    let registry = RegistryHandle::new(registry);
    seed::spawn_config_seeding(
        &cache,
        &registry,
        &config,
        load.seed,
        &cancel,
        polling_watch,
        &handle,
    );

    match polling_watch {
        // Daemon path: probe fs-event delivery before trusting the native
        // backend (canon provider_source.md §"Watch backend health").
        None => scheduler.self_test_watch_backend(),
        Some(interval) => scheduler.use_polling_watcher(interval),
    }
    let heartbeat = scheduler.heartbeat();

    let scheduler_handle = handle.clone();
    let scheduler_task = tokio::spawn(async move { scheduler.run().await });

    // Start watchdog if configured.
    let watchdog_task = spawn_watchdog(&config, heartbeat, cancel.clone());

    let mut server = Server::new_with_config(
        socket_path,
        cache,
        registry,
        Some(handle),
        watchers,
        config.clone(),
    );
    if let Some(health) = reaper {
        server = server.with_reaper_health(health);
    }
    if let Some(procs) = procs {
        server = server.with_process_snapshotter(procs);
    }

    // Bind before serving so readiness can be signalled: whoever holds the
    // `DaemonReady` connects the moment the socket is live instead of guessing.
    let bound = server.bind();
    if bound.is_ok() {
        let _ = ready.send(());
    }

    tokio::select! {
        result = async { server.serve(bound?).await } => {
            if let Err(e) = result {
                tracing::error!("Server error: {}", e);
            }
            // Server returned (bind failure or accept-loop error): the
            // scheduler goes too (the cancel token has no remaining observer
            // once this select ends).
        }
        _ = cancel.cancelled() => {
            info!("Shutdown signal received");
        }
    }

    if let Some(task) = watchdog_task {
        task.abort();
    }
    if stop_scheduler(&scheduler_handle, scheduler_task).await {
        info!("Daemon shut down cleanly");
    } else {
        warn!("the scheduler did not stop within {SCHEDULER_STOP:?}; leaving it");
    }
}

/// How long a stopping daemon waits for its scheduler, which ends the moment
/// it reads its message. Past this it is wedged, and the process goes without
/// it: a daemon that cannot be stopped is one its supervisor must kill.
const SCHEDULER_STOP: std::time::Duration = std::time::Duration::from_secs(2);

/// Ask the scheduler to stop and wait for it, for [`SCHEDULER_STOP`] at most:
/// whether it did.
async fn stop_scheduler(
    handle: &crate::scheduler::SchedulerHandle,
    task: tokio::task::JoinHandle<()>,
) -> bool {
    handle.send(SchedulerMessage::Shutdown).await;
    tokio::time::timeout(SCHEDULER_STOP, task).await.is_ok()
}

/// What a daemon refusing its configuration says: one line per error, then
/// the verdict. The same lines whether it refuses before its logger is built
/// (`comb daemon`, on stderr) or after it (in the log).
pub fn refusal(errors: &[String]) -> Vec<String> {
    let mut lines: Vec<String> = errors.iter().map(|e| format!("error: {e}")).collect();
    lines.push(format!(
        "error: daemon: refusing to start: the configuration has {} error(s)",
        errors.len()
    ));
    lines
}

fn refuse_to_start(errors: &[String]) {
    for line in refusal(errors) {
        tracing::error!("{line}");
    }
}

/// Spawn a watchdog task that monitors the scheduler heartbeat.
/// On stall detection, cancels the daemon's CancellationToken to trigger a clean
/// shutdown. The process supervisor (launchd, systemd, etc.) handles restart.
/// Returns None if watchdog is not configured.
fn spawn_watchdog(
    config: &Config,
    heartbeat: Arc<AtomicU64>,
    cancel: CancellationToken,
) -> Option<tokio::task::JoinHandle<()>> {
    let check_interval = config.daemon().watchdog_interval_duration()?;
    // An interval has a threshold (its own, or 3x); `Config` refuses a
    // threshold without one.
    let threshold = config.daemon().watchdog_threshold_duration()?;

    info!(
        "Watchdog enabled: checking every {}, threshold {}",
        libbeachcomber::render::duration(check_interval),
        libbeachcomber::render::duration(threshold)
    );

    Some(tokio::spawn(async move {
        let mut tick = tokio::time::interval(check_interval);
        let mut last_seen_beat: u64 = heartbeat.load(Ordering::Relaxed);
        let mut stale_since: Option<tokio::time::Instant> = None;

        loop {
            tick.tick().await;
            let current_beat = heartbeat.load(Ordering::Relaxed);

            if current_beat != last_seen_beat {
                last_seen_beat = current_beat;
                stale_since = None;
            } else {
                let stale_start = *stale_since.get_or_insert(tokio::time::Instant::now());
                let stale_duration = stale_start.elapsed();

                if stale_duration >= threshold {
                    warn!(
                        "Watchdog: scheduler heartbeat stale for {:?} (threshold {:?}), triggering shutdown",
                        stale_duration, threshold
                    );
                    cancel.cancel();
                    break;
                }
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scheduler that takes nothing and never ends does not keep the daemon
    /// from stopping: this wait, unbounded, is what SIGTERM got stuck on.
    #[tokio::test(start_paused = true)]
    async fn a_wedged_scheduler_does_not_hold_the_daemons_stop() {
        let (tx, _never_drained) = tokio::sync::mpsc::channel(1);
        let handle = crate::scheduler::SchedulerHandle::new(tx);
        handle.send(SchedulerMessage::Shutdown).await;
        let wedged = tokio::spawn(std::future::pending());
        assert!(!stop_scheduler(&handle, wedged).await);
    }

    #[tokio::test]
    async fn a_scheduler_that_ends_stops_the_daemon_at_once() {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let handle = crate::scheduler::SchedulerHandle::new(tx);
        assert!(stop_scheduler(&handle, tokio::spawn(async {})).await);
    }
}
