//! Handler for the `daemon` subcommand.

use crate::config::Config;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

pub fn run_daemon(socket_path: PathBuf, config: Config) -> ExitCode {
    let log_path = config.resolve_log_path();

    // Ensure log directory exists.
    if let Some(parent) = log_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    // Open log file (append mode).
    let log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path);

    let filter: tracing_subscriber::filter::LevelFilter = config
        .daemon
        .log_level
        .parse()
        .unwrap_or(tracing_subscriber::filter::LevelFilter::INFO);
    let env_filter = EnvFilter::from_default_env().add_directive(filter.into());

    match log_file {
        Ok(file) => {
            // Log to both stderr and file.
            let stderr_layer = fmt::layer().with_target(true).with_writer(std::io::stderr);
            let file_layer = fmt::layer()
                .with_target(true)
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(file));

            tracing_subscriber::registry()
                .with(env_filter)
                .with(stderr_layer)
                .with(file_layer)
                .init();
        }
        Err(_) => {
            // Fall back to stderr only.
            tracing_subscriber::fmt().with_max_level(filter).init();
        }
    }

    tracing::info!("Starting beachcomber daemon");
    tracing::info!("Socket: {:?}", socket_path);
    tracing::info!("Log file: {:?}", log_path);

    let pid_path = socket_path.with_file_name("pid");
    let binary_hash = match crate::singleton::hash_current_binary() {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("failed to hash current binary for singleton identity: {e}");
            return ExitCode::FAILURE;
        }
    };
    let _singleton_lock = match crate::singleton::acquire_or_supersede(
        &pid_path,
        &socket_path,
        env!("BEACHCOMBER_VERSION"),
        &binary_hash,
    ) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            tracing::info!(
                "another daemon with the same binary is already running; exiting silently"
            );
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            tracing::error!("failed to acquire singleton lock: {e}");
            return ExitCode::FAILURE;
        }
    };

    // No separate reap step here: `acquire_or_supersede` handles all contention.
    // A different-build owner is superseded; a same-build owner is left alone if
    // it is serving the socket, or superseded (after a short serving-probe grace)
    // if it is wedged before bind. Startup orphan-reaping by binary path was
    // removed (it also killed peer daemons on other socket paths). See
    // `docs/canon/singleton.md` §"Same-build serving probe".

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    rt.block_on(async {
        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_clone = cancel.clone();

        tokio::spawn(async move {
            use tokio::signal::unix::{SignalKind, signal};
            let mut sigint = match signal(SignalKind::interrupt()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("Failed to install SIGINT handler: {e}");
                    return;
                }
            };
            let mut sigterm = match signal(SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("Failed to install SIGTERM handler: {e}");
                    return;
                }
            };
            tokio::select! {
                _ = sigint.recv() => tracing::info!("Received SIGINT, shutting down..."),
                _ = sigterm.recv() => tracing::info!("Received SIGTERM, shutting down..."),
            }
            cancel_clone.cancel();
        });

        let cancel_for_self_watch = cancel.clone();
        if let Err(e) = crate::singleton::spawn_binary_self_watch(move || {
            cancel_for_self_watch.cancel();
        }) {
            tracing::warn!(
                "failed to start binary self-watch: {e} (binary updates won't trigger restart)"
            );
        }

        let our_start_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        if let Ok(exe) = std::env::current_exe() {
            match crate::singleton::binary_newer_than(&exe, our_start_ms) {
                Ok(true) => {
                    tracing::warn!(
                        "binary was modified between exec and watch registration; shutting down for restart"
                    );
                    cancel.cancel();
                }
                Ok(false) => {}  // common case — proceed
                Err(e) => {
                    tracing::warn!("could not stat binary for race-check: {e}");
                }
            }
        }

        let handle = crate::daemon::start_in_process_with_cancel(socket_path, config, cancel);
        handle.await.ok();
    });

    ExitCode::SUCCESS
}
