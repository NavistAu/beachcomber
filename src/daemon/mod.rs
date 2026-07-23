pub mod lifecycle;

use crate::boundaries::spawn::{DaemonSpawner, RealDaemonSpawner};
use crate::cache::Cache;
use crate::config::Config;
use crate::provider::registry::ProviderRegistry;
use crate::scheduler::{Scheduler, SchedulerMessage};
use crate::server::Server;
use crate::watcher_registry::WatcherRegistry;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// Pure-logic re-exports for callers that import directly from `crate::daemon`.
// ---------------------------------------------------------------------------

/// Derive the PID-file path for a given socket path.
///
/// Delegates to the pure implementation in `lifecycle`.
pub fn pid_path_for_socket(socket_path: &Path) -> PathBuf {
    lifecycle::pid_path_for_socket(socket_path)
}

// ---------------------------------------------------------------------------
// OS-bound: socket connectivity check
// ---------------------------------------------------------------------------

pub fn is_daemon_running(socket_path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket_path).is_ok()
}

// ---------------------------------------------------------------------------
// OS-bound: in-process daemon startup (used by tests and embedded mode)
// ---------------------------------------------------------------------------

pub fn start_in_process(socket_path: PathBuf, config: Config) -> tokio::task::JoinHandle<()> {
    let cancel = CancellationToken::new();
    start_in_process_with_cancel(socket_path, config, cancel)
}

pub fn start_in_process_with_cancel(
    socket_path: PathBuf,
    config: Config,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    start_in_process_with_reaper(socket_path, config, cancel, None)
}

/// Daemon-path variant: also attaches reaper health for introspect surfacing
/// (canon singleton.md invariant 13).
pub fn start_in_process_with_reaper(
    socket_path: PathBuf,
    config: Config,
    cancel: CancellationToken,
    reaper: Option<std::sync::Arc<crate::singleton::ReaperHealth>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        run_daemon_with_cancel(socket_path, config, cancel, reaper).await;
    })
}

async fn run_daemon_with_cancel(
    socket_path: PathBuf,
    config: Config,
    cancel: CancellationToken,
    reaper: Option<std::sync::Arc<crate::singleton::ReaperHealth>>,
) {
    // Load env file before anything else — providers need these vars
    let env_count = config.load_env_file();
    if env_count > 0 {
        info!("Loaded {} environment variables from env file", env_count);
    }

    let watchers = Arc::new(WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(ProviderRegistry::with_config(&config));

    // Validate provider config blocks against registered sources.
    // Errors → refuse to start. Warnings → log and continue.
    {
        let known_providers = registry.provider_names();
        let known_sources: std::collections::HashMap<String, Vec<String>> = known_providers
            .iter()
            .filter_map(|p| {
                registry
                    .provider_sources(p)
                    .map(|sources| (p.clone(), sources.iter().map(|s| s.name.clone()).collect()))
            })
            .collect();
        let (warnings, errors) = config.validate_providers(&known_providers, &known_sources);
        for w in &warnings {
            tracing::warn!("{}", w);
        }
        if !errors.is_empty() {
            for e in &errors {
                tracing::error!("{}", e);
            }
            tracing::error!(
                "Config validation failed with {} error(s). Daemon refusing to start.",
                errors.len()
            );
            return;
        }
    }

    let (handle, mut scheduler) = Scheduler::new(
        cache.clone(),
        registry.clone(),
        config.clone(),
        watchers.clone(),
    );
    // Daemon path: probe fs-event delivery before trusting the native backend
    // (canon singleton.md §"Watch self-test").
    scheduler.self_test_watch_backend();
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

    tokio::select! {
        result = server.run() => {
            if let Err(e) = result {
                tracing::error!("Server error: {}", e);
            }
        }
        _ = cancel.cancelled() => {
            info!("Shutdown signal received");
            scheduler_handle.send(SchedulerMessage::Shutdown).await;
        }
    }

    if let Some(task) = watchdog_task {
        task.abort();
    }
    let _ = scheduler_task.await;
    info!("Daemon shut down cleanly");
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
    let check_interval = config.daemon.watchdog_interval_duration()?;
    let threshold = config
        .daemon
        .watchdog_threshold_duration()
        .unwrap_or(check_interval * 3);

    info!(
        "Watchdog enabled: checking every {:?}, threshold {:?}",
        check_interval, threshold
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

// ---------------------------------------------------------------------------
// OS-bound: fork + wait
// ---------------------------------------------------------------------------

/// Fork a daemon process. Called by `RealDaemonSpawner::fork_daemon`.
pub fn fork_daemon(binary_path: &str, socket_path: &Path) -> std::io::Result<()> {
    use std::process::Command;

    let pid_path = pid_path_for_socket(socket_path);

    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Open log file for the forked daemon's stderr.
    let config = crate::config::Config::load();
    let log_path = config.resolve_log_path();
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .unwrap_or_else(|_| {
            // Fall back to /dev/null if log file can't be opened.
            std::fs::File::open("/dev/null").unwrap()
        });

    let child = Command::new(binary_path)
        .arg("daemon")
        .arg("--socket")
        .arg(socket_path.as_os_str())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(log_file))
        .spawn()?;

    std::fs::write(&pid_path, child.id().to_string())?;

    Ok(())
}

/// Poll the socket until the daemon accepts connections or the attempt budget is exhausted.
/// Called by `RealDaemonSpawner::wait_for_socket`.
pub fn wait_for_daemon(socket_path: &Path, max_attempts: u32) -> bool {
    use lifecycle::{WaitDecision, next_wait_decision};

    let mut delay_ms = 10u64;
    for attempt in 0..=max_attempts {
        if is_daemon_running(socket_path) {
            return true;
        }
        match next_wait_decision(attempt, max_attempts, delay_ms) {
            WaitDecision::Ready => return true,
            WaitDecision::Timeout => return false,
            WaitDecision::Sleep(ms) => {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                delay_ms = ms;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// ensure_daemon — public API with injection-point variant
// ---------------------------------------------------------------------------

/// Ensure the daemon is running at `socket_path`.
///
/// If not running, forks it and waits up to 8 attempts for the socket to appear.
/// This is the backwards-compatible entry point; it uses `RealDaemonSpawner`.
pub fn ensure_daemon(socket_path: &Path) -> std::io::Result<()> {
    ensure_daemon_with(&RealDaemonSpawner, socket_path)
}

/// Injection-point variant of `ensure_daemon` — accepts any `DaemonSpawner`.
///
/// Useful for tests: pass a `MockDaemonSpawner` to assert fork/wait behaviour
/// without touching the filesystem.
pub fn ensure_daemon_with(spawner: &dyn DaemonSpawner, socket_path: &Path) -> std::io::Result<()> {
    if !lifecycle::needs_fork(is_daemon_running(socket_path)) {
        return Ok(());
    }

    let binary = std::env::current_exe()?.to_string_lossy().to_string();

    spawner.fork_daemon(&binary, socket_path)?;

    if !spawner.wait_for_socket(socket_path, 8) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "Daemon failed to start within timeout",
        ));
    }

    Ok(())
}
