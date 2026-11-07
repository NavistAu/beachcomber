//! Handler for the `daemon` subcommand.

use crate::cli::exit::Outcome;
use crate::config::Config;
use std::fs;
use std::io::{IsTerminal, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::path::PathBuf;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

pub fn run_daemon(
    socket_path: PathBuf,
    mut config: Config,
    exit_with_parent: bool,
    ready_fd: Option<RawFd>,
) -> Outcome {
    // Captured before any startup work: a parent that dies during startup must
    // still be noticed, and once we are re-parented getppid() no longer names it.
    let parent_pid = unsafe { libc::getppid() };
    // A daemon outlives whoever reads what it writes: a reader gone away (a
    // spawner that stopped waiting on `--ready-fd`) is an EPIPE on the write,
    // never a signal that kills it. A command it runs starts with the default.
    // SAFETY: setting a disposition to SIG_IGN installs no handler.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    let ready = match ready_fd.map(Ready::adopt).transpose() {
        Ok(ready) => ready,
        Err(e) => {
            crate::cli::report::error("daemon", e);
            return Outcome::Error;
        }
    };
    let log_path = Config::log_path();
    config.persist_puts();

    // A configuration with errors is refused before anything is set up: the
    // log level it names is not trusted to build the logger.
    let errors = config.errors();
    if !errors.is_empty() {
        for line in crate::daemon::refusal(&errors) {
            eprintln!("{line}");
        }
        return Outcome::Error;
    }

    // Ensure log directory exists.
    if let Some(parent) = log_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    // Open log file (append mode).
    let log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path);

    let filter = config.daemon().log_level;
    let env_filter = EnvFilter::from_default_env()
        .add_directive(filter.into())
        // PollWatcher warns on every file that vanishes mid-walk.
        .add_directive("notify::poll=error".parse().expect("static directive"));

    match log_file {
        Ok(file) => {
            // Log to both stderr and file.
            // A spawned daemon's stderr is `daemon.log`: escapes only for a
            // terminal.
            let stderr_layer = fmt::layer()
                .with_target(true)
                .with_ansi(std::io::stderr().is_terminal())
                .with_writer(std::io::stderr);
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
            tracing_subscriber::fmt()
                .with_max_level(filter)
                .with_ansi(std::io::stderr().is_terminal())
                .init();
        }
    }

    tracing::info!("Starting beachcomber daemon");
    tracing::info!("Socket: {:?}", socket_path);
    tracing::info!("Log file: {:?}", log_path);

    if exit_with_parent && parent_pid == 1 {
        tracing::info!("--exit-with-parent but the parent is already gone (ppid 1); exiting");
        return Outcome::Done;
    }

    let pid_path = socket_path.with_file_name("pid");
    let binary_hash = match crate::singleton::hash_current_binary() {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("failed to hash current binary for singleton identity: {e}");
            return Outcome::Error;
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
            // It serves this socket, which is what a spawner waits to hear.
            if let Some(ready) = ready {
                ready.signal();
            }
            return Outcome::Done;
        }
        Err(e) => {
            tracing::error!("failed to acquire singleton lock: {e}");
            return Outcome::Error;
        }
    };

    // No separate reap step here: `acquire_or_supersede` handles all contention.
    // A different-build owner is superseded; a same-build owner is left alone if
    // it is serving the socket, or superseded (after a short serving-probe grace)
    // if it is wedged before bind. Startup orphan-reaping by binary path was
    // removed (it also killed peer daemons on other socket paths). See
    // `docs/canon/singleton.md` §"Same-build serving probe".

    // The env file becomes the daemon's own environment, which every Source
    // reads and every command it runs inherits. Setting it is sound only while
    // the process has one thread, so it is loaded here, once, before the
    // runtime below starts any.
    // SAFETY: nothing so far has started a thread; the runtime is built next.
    let env_count = unsafe { config.load_env_file() };
    if env_count > 0 {
        tracing::info!("Loaded {env_count} environment variables from env file");
    }

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    let outcome = rt.block_on(async {
        let cancel = tokio_util::sync::CancellationToken::new();

        if let Err(e) = crate::daemon::spawn_signal_thread(
            cancel.clone(),
            crate::daemon::STOP_GRACE,
            || {
                tracing::error!("still running {:?} after a stop signal, exiting", crate::daemon::STOP_GRACE);
                // SAFETY: ends the process at once; a wedged daemon must not
                // wait on anything on its way out.
                unsafe { libc::_exit(1) }
            },
        ) {
            tracing::error!("Failed to install signal handlers: {e}");
        }

        // Tie our lifetime to the spawning process when asked (tests). Survives a
        // SIGKILL of the parent, where Drop-based cleanup in the spawner cannot run.
        if exit_with_parent {
            spawn_parent_death_watch(cancel.clone(), parent_pid);
        }

        let cancel_for_self_watch = cancel.clone();
        if let Err(e) = crate::singleton::spawn_binary_self_watch(move || {
            cancel_for_self_watch.cancel();
        }) {
            tracing::warn!(
                "failed to start binary self-watch: {e} (binary updates won't trigger restart)"
            );
        }

        // The config self-watch is spawned by `run_daemon_with_cancel`, which
        // owns the cache and registry a providers change reloads in place.

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

        // Orphan reaping — reaping daemon only (canon §"Who reaps"). The
        // reaper role is decided against the ENV-FREE resolution (config →
        // default, ignoring --socket and $BEACHCOMBER_SOCKET): env is
        // per-process, and honoring it here lets two daemons of one uid each
        // self-assess "canonical" and reap each other (fratricide). The
        // interval's first tick fires immediately (startup sweep), then
        // hourly. Sweeps run on the blocking pool: the process walk and the
        // SIGTERM grace waits are synchronous. Reaper health is surfaced via
        // introspect/status/check (canon invariant 13).
        let reaper_health = std::sync::Arc::new(crate::singleton::ReaperHealth::default());
        if crate::singleton::policy::is_canonical_daemon(
            &socket_path,
            &config.resolve_reaper_socket_path(),
        ) {
            use std::sync::atomic::Ordering::Relaxed;
            reaper_health.armed.store(true, Relaxed);

            // Visibility self-test at arming (canon §"Reaper visibility
            // self-test") — warn once here, not per sweep.
            {
                use crate::boundaries::proc_table::{ProcessTable, RealProcessTable};
                let visible = RealProcessTable.pid1_visible();
                reaper_health.visibility_ok.store(visible, Relaxed);
                if !visible {
                    tracing::warn!(
                        "reaper visibility degraded: PID 1 not visible in process enumeration — \
                         this daemon runs confined and cannot police daemons outside its confinement"
                    );
                }
            }

            let reap_socket = socket_path.clone();
            let reap_cancel = cancel.clone();
            let reap_health = reaper_health.clone();
            tokio::spawn(async move {
                let mut interval =
                    tokio::time::interval(std::time::Duration::from_secs(60 * 60));
                loop {
                    tokio::select! {
                        _ = reap_cancel.cancelled() => break,
                        _ = interval.tick() => {
                            let sock = reap_socket.clone();
                            if let Ok(report) = tokio::task::spawn_blocking(move || {
                                crate::singleton::reap_sweep(&sock)
                            })
                            .await
                            {
                                reap_health.record_sweep(&report);
                            }
                        }
                    }
                }
            });
        }

        let (handle, serving) = crate::daemon::start_in_process_with_reaper(
            socket_path,
            config,
            cancel,
            Some(reaper_health),
        );
        // The socket is bound the moment `serving` resolves: say so on
        // `--ready-fd`. A daemon that never serves drops it unwritten.
        let served = tokio::spawn(async move {
            let served = serving.await.is_ok();
            if let Some(ready) = ready.filter(|_| served) {
                ready.signal();
            }
            served
        });
        handle.await.ok();
        // A daemon that never signalled readiness never served: config
        // validation refused it or the bind failed. Exit 2, as a failed
        // managed-defaults write does, so a supervisor can tell a refused
        // start from a clean stop.
        if !served.await.unwrap_or(false) {
            return Outcome::Error;
        }
        Outcome::Done
    });
    // The daemon has shut down. A Source's run that was cut off at its
    // timeout may still be waiting on its command on a blocking thread:
    // dropping the runtime would wait for that command to end, so the
    // process leaves it instead.
    rt.shutdown_background();
    outcome
}

/// The pipe a spawner handed this daemon (`--ready-fd`), written once the
/// socket is served.
struct Ready(std::fs::File);

impl Ready {
    /// Take `fd` over, close-on-exec, so no command the daemon runs holds it
    /// open past the daemon's exit. A number that names no open descriptor
    /// is refused.
    fn adopt(fd: RawFd) -> Result<Self, String> {
        // SAFETY: `fcntl` only reads and sets the descriptor's flags; it
        // fails with EBADF on a number that is not open.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
            return Err(format!(
                "--ready-fd {fd}: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: `fd` is open, and was handed to this process to own.
        Ok(Self(unsafe { std::fs::File::from_raw_fd(fd) }))
    }

    /// Write the newline and close the pipe. A spawner that stopped waiting
    /// is no concern of the daemon's.
    fn signal(mut self) {
        let _ = self.0.write_all(b"\n");
    }
}

/// How often `--exit-with-parent` looks at its parent: one `getppid()`, so a
/// test that kills the parent sees the daemon go within tens of milliseconds.
const PARENT_CHECK: std::time::Duration = std::time::Duration::from_millis(50);

/// Cancel `cancel` when this daemon's parent process dies, detected via
/// re-parenting: `getppid()` changes from `initial_ppid`, captured at the top of
/// `run_daemon` (so a parent that died mid-startup is caught at once; the
/// kernel re-parents orphans to launchd/init). Backs `--exit-with-parent` so a
/// test-spawned daemon does not leak when its spawner is SIGKILLed without
/// running cleanup — the cause of the 48-day orphaned-daemon pile-up that bloated
/// `fseventsd`. Polls rather than using a signal so it is portable (macOS has no
/// `PR_SET_PDEATHSIG`).
fn spawn_parent_death_watch(cancel: tokio_util::sync::CancellationToken, initial_ppid: i32) {
    std::thread::spawn(move || {
        loop {
            let current_ppid = unsafe { libc::getppid() };
            if crate::daemon::lifecycle::should_exit_on_reparent(initial_ppid, current_ppid) {
                tracing::info!(
                    "parent process exited (ppid {initial_ppid} -> {current_ppid}); shutting down (--exit-with-parent)"
                );
                cancel.cancel();
                break;
            }
            std::thread::sleep(PARENT_CHECK);
        }
    });
}
