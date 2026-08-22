//! Config-file self-watch: mirrors the binary self-watch
//! (`crate::singleton::spawn_binary_self_watch_with`) — an fs-event watch as
//! the fast path, a guaranteed mtime poll as the backstop, 200ms debounce —
//! but gates the restart on the new file actually parsing. See canon
//! `docs/canon/singleton.md` §"Self-supervision".
//!
//! The watch targets `Config::config_path()`: the deterministic XDG
//! location, not gated on the file existing at daemon startup. Because the
//! watch is against a *path* rather than an open file, a config file that
//! doesn't exist yet when the daemon starts is picked up naturally once it's
//! created — the mtime poll's `stat` starts succeeding once the file exists,
//! and the fs-event watch (registered on the parent directory, same as the
//! binary watch) delivers a Create event the same way it would for a binary
//! replacement.
//!
//! Known limitation: if the config *directory* doesn't exist yet at daemon
//! startup, the fs-event watch can't be registered against a missing
//! directory (falls back to poll-only), and the poll's `stat` keeps failing
//! (`NotFound`) until the directory exists too — so a config file created
//! inside a directory that didn't exist at daemon startup is not picked up
//! until some other restart trigger fires (binary self-watch, manual
//! restart, `comb daemon` re-invocation).

use crate::config::Config;
use crate::daemon::lifecycle::should_restart_for_config_change;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Config self-watch poll interval — same 5s guarantee as the binary watch.
pub const CONFIG_WATCH_POLL_INTERVAL: Duration = crate::singleton::SELF_WATCH_POLL_INTERVAL;

/// Spawn a thread that watches the daemon's resolved config file. When it
/// changes and the new content parses as valid config, calls `on_change`
/// once (after a 200ms debounce) and exits. When it changes but fails to
/// parse, logs a warning naming the error and keeps watching — never
/// restarts into a config that would fail startup.
pub fn spawn_config_self_watch<F: FnOnce() + Send + 'static>(on_change: F) {
    spawn_config_self_watch_with(
        Config::config_path(),
        CONFIG_WATCH_POLL_INTERVAL,
        true,
        on_change,
    );
}

/// Parameterised variant: explicit path, poll interval, and whether to
/// attempt the fs-event fast path (`fs_events: false` is the degraded mode
/// the mtime poll exists for). Tests use this seam directly; production goes
/// through [`spawn_config_self_watch`].
pub fn spawn_config_self_watch_with<F: FnOnce() + Send + 'static>(
    config_path: PathBuf,
    poll_interval: Duration,
    fs_events: bool,
    on_change: F,
) {
    use notify::{EventKind, RecursiveMode, Watcher};
    use std::sync::mpsc;

    std::thread::spawn(move || {
        let mut baseline_ms = now_unix_ms();

        let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
        let watcher = if fs_events {
            match (notify::recommended_watcher(tx), config_path.parent()) {
                (Ok(mut w), Some(parent)) => match w.watch(parent, RecursiveMode::NonRecursive) {
                    Ok(()) => {
                        tracing::debug!(
                            "config-watch: watching {parent:?} for changes to {config_path:?}"
                        );
                        Some(w)
                    }
                    Err(e) => {
                        tracing::warn!("config-watch: failed to watch {parent:?}: {e}; poll only");
                        None
                    }
                },
                (Err(e), _) => {
                    tracing::warn!("config-watch: failed to create fs watcher: {e}; poll only");
                    None
                }
                (_, None) => {
                    tracing::warn!(
                        "config-watch: config path has no parent: {config_path:?}; poll only"
                    );
                    None
                }
            }
        } else {
            None
        };

        if watcher.is_none() {
            poll_loop(&config_path, poll_interval, baseline_ms, on_change);
            return;
        }
        let _watcher = watcher;

        let debounce = Duration::from_millis(200);
        let mut last_event: Option<Instant> = None;
        let mut next_poll = Instant::now() + poll_interval;
        let mut on_change = Some(on_change);

        loop {
            let now = Instant::now();
            let debounce_deadline = last_event.map(|t| t + debounce);
            let wake = match debounce_deadline {
                Some(d) => d.min(next_poll),
                None => next_poll,
            };

            match rx.recv_timeout(wake.saturating_duration_since(now)) {
                Ok(Ok(event)) => {
                    tracing::debug!(
                        "config-watch event: kind={:?} paths={:?}",
                        event.kind,
                        event.paths
                    );
                    let target = config_path
                        .canonicalize()
                        .unwrap_or_else(|_| config_path.clone());
                    let path_match = event.paths.iter().any(|p| {
                        let canonical = p.canonicalize().unwrap_or_else(|_| p.clone());
                        canonical == target || p == &config_path
                    });
                    if path_match {
                        let is_change = matches!(
                            event.kind,
                            EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                        );
                        if is_change {
                            last_event = Some(Instant::now());
                        }
                    }
                }
                Ok(Err(e)) => {
                    tracing::error!("config fs-watch error: {e}");
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Some(d) = debounce_deadline
                        && Instant::now() >= d
                    {
                        last_event = None;
                        if try_restart(&config_path, &mut on_change) {
                            return; // one-shot
                        }
                        baseline_ms = now_unix_ms();
                    }
                    if Instant::now() >= next_poll {
                        next_poll = Instant::now() + poll_interval;
                        if matches!(
                            crate::singleton::binary_newer_than(&config_path, baseline_ms),
                            Ok(true)
                        ) {
                            if try_restart(&config_path, &mut on_change) {
                                return; // one-shot
                            }
                            baseline_ms = now_unix_ms();
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // Event channel gone mid-run; the poll carries on alone.
                    let Some(f) = on_change.take() else {
                        return;
                    };
                    poll_loop(&config_path, poll_interval, baseline_ms, f);
                    return;
                }
            }
        }
    });
}

/// Poll fallback used both when the fs-event watcher couldn't be set up at
/// all, and when its channel disconnects mid-run. Loops until a change is
/// detected AND validates cleanly; an invalid change is logged and polling
/// continues.
fn poll_loop<F: FnOnce()>(
    config_path: &Path,
    poll_interval: Duration,
    start_baseline_ms: u64,
    on_change: F,
) {
    let mut on_change = Some(on_change);
    let mut baseline_ms = start_baseline_ms;
    loop {
        std::thread::sleep(poll_interval);
        if matches!(
            crate::singleton::binary_newer_than(config_path, baseline_ms),
            Ok(true)
        ) {
            if try_restart(config_path, &mut on_change) {
                return;
            }
            baseline_ms = now_unix_ms();
        }
    }
}

/// Validate the file at `config_path` and, if it parses cleanly, consume
/// `on_change` and fire it (returns `true` — caller must stop watching). If
/// it fails to parse (including "file doesn't exist", e.g. deleted), logs a
/// warning naming the error and returns `false` — caller keeps watching.
fn try_restart<F: FnOnce()>(config_path: &Path, on_change: &mut Option<F>) -> bool {
    let result = validate_config_file(config_path);
    if should_restart_for_config_change(&result) {
        tracing::info!(
            "config file changed and parses cleanly; initiating graceful shutdown for restart"
        );
        if let Some(f) = on_change.take() {
            f();
        }
        true
    } else {
        let e = result.unwrap_err();
        tracing::warn!(
            "config file changed but is not valid ({e}); keeping current config, not restarting"
        );
        false
    }
}

fn validate_config_file(path: &Path) -> Result<(), String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("could not read config file: {e}"))?;
    Config::parse_str(&content).map(|_| ())
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
