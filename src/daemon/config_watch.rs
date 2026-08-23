//! Config-file self-watch: mirrors the binary self-watch
//! (`crate::singleton::spawn_binary_self_watch_with`) — an fs-event watch as
//! the fast path, a guaranteed mtime poll as the backstop, 200ms debounce —
//! but gates the restart on the new file actually parsing. Config
//! self-supervision is deliberately not part of the singleton canon (that spec
//! covers binary freshness); the user-facing contract lives in the website
//! configuration reference.
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
//! Coverage also extends to the conf.d convention (`Config::conf_d_dir_for`,
//! `docs/reference/configuration-reference.md` §"Composition / conf.d"): the
//! composed set is main config file + every `conf.d/*.toml`. A change to any
//! file in that set — the main file OR a conf.d drop-in modified, added, or
//! removed — runs through the same debounce and parse-gate, validating the
//! WHOLE composed set (`Config::parse_composed`) before restarting. Because
//! conf.d is a subdirectory, not a sibling of the config file, it needs its
//! own fs-event registration (the parent-dir watch is `NonRecursive` and
//! doesn't see inside it) — registered separately, best-effort, when the
//! directory exists at watch-registration time. The mtime-poll backstop
//! checks the newest mtime across the whole composed file set, plus the
//! file COUNT, so a drop-in's deletion (which doesn't bump any remaining
//! file's mtime) is still noticed.
//!
//! Known limitation: if the config *directory* doesn't exist yet at daemon
//! startup, the fs-event watch can't be registered against a missing
//! directory (falls back to poll-only), and the poll's `stat` keeps failing
//! (`NotFound`) until the directory exists too — so a config file created
//! inside a directory that didn't exist at daemon startup is not picked up
//! until some other restart trigger fires (binary self-watch, manual
//! restart, `comb daemon` re-invocation). The same limitation applies to
//! conf.d specifically: if `conf.d/` doesn't exist yet at watch-registration
//! time, no fs-event watch is registered for it (there's nothing to watch),
//! so its later creation and population is only picked up by the poll
//! backstop (file-count delta), not the fast path — same honest tradeoff as
//! the config directory case above, just one level down.

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

/// Rolling reference point for the mtime-poll backstop: `since_ms` is the
/// wall-clock instant beyond which a composed-set file's mtime counts as
/// "changed" (same idea as the binary self-watch's baseline), and `count` is
/// the last-observed total file count across the composed set (main config
/// file, if present, plus every `conf.d/*.toml`) — tracked separately because
/// a deletion doesn't bump any remaining file's mtime.
#[derive(Debug, Clone, Copy)]
struct Baseline {
    since_ms: u64,
    count: usize,
}

/// Current on-disk state of the composed config set: the newest mtime among
/// the main config file (if it exists) and every `conf.d/*.toml` (0 if none
/// exist), and the total file count.
fn composed_state(config_path: &Path, conf_d_dir: &Path) -> (u64, usize) {
    let mut newest_ms = 0u64;
    let mut count = 0usize;
    let mut note = |p: &Path| {
        if let Ok(meta) = std::fs::metadata(p) {
            count += 1;
            if let Ok(mtime) = meta.modified() {
                newest_ms = newest_ms.max(mtime_to_unix_ms(mtime));
            }
        }
    };
    note(config_path);
    for f in Config::conf_d_files(conf_d_dir) {
        note(&f);
    }
    (newest_ms, count)
}

/// True if the composed set has changed relative to `baseline`: some file's
/// mtime is newer than `baseline.since_ms`, or the file count differs (a
/// drop-in was added or removed).
fn composed_changed(config_path: &Path, conf_d_dir: &Path, baseline: Baseline) -> bool {
    let (newest_ms, count) = composed_state(config_path, conf_d_dir);
    newest_ms > baseline.since_ms || count != baseline.count
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
        let conf_d_dir = Config::conf_d_dir_for(&config_path);
        let (_, initial_count) = composed_state(&config_path, &conf_d_dir);
        let mut baseline = Baseline {
            since_ms: now_unix_ms(),
            count: initial_count,
        };

        let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
        let watcher = if fs_events {
            match (notify::recommended_watcher(tx), config_path.parent()) {
                (Ok(mut w), Some(parent)) => match w.watch(parent, RecursiveMode::NonRecursive) {
                    Ok(()) => {
                        tracing::debug!(
                            "config-watch: watching {parent:?} for changes to {config_path:?}"
                        );
                        // conf.d is a subdirectory, not a sibling — the parent-dir
                        // watch above is NonRecursive and can't see inside it, so
                        // it needs its own registration. Best-effort: only
                        // possible if the directory already exists, and its
                        // absence doesn't invalidate the rest of the watch (the
                        // poll backstop covers conf.d either way).
                        if conf_d_dir.is_dir() {
                            match w.watch(&conf_d_dir, RecursiveMode::NonRecursive) {
                                Ok(()) => tracing::debug!(
                                    "config-watch: watching conf.d dir {conf_d_dir:?}"
                                ),
                                Err(e) => tracing::warn!(
                                    "config-watch: failed to watch conf.d dir {conf_d_dir:?}: {e}; \
                                     conf.d changes rely on the poll backstop"
                                ),
                            }
                        }
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
            poll_loop(
                &config_path,
                &conf_d_dir,
                poll_interval,
                baseline,
                on_change,
            );
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
                    let conf_d_target = conf_d_dir
                        .canonicalize()
                        .unwrap_or_else(|_| conf_d_dir.clone());
                    let path_match = event.paths.iter().any(|p| {
                        let canonical = p.canonicalize().unwrap_or_else(|_| p.clone());
                        if canonical == target || p == &config_path {
                            return true;
                        }
                        let is_toml = p.extension().and_then(|e| e.to_str()) == Some("toml");
                        is_toml
                            && (p.parent() == Some(conf_d_dir.as_path())
                                || canonical.parent() == Some(conf_d_target.as_path()))
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
                        baseline = fresh_baseline(&config_path, &conf_d_dir);
                    }
                    if Instant::now() >= next_poll {
                        next_poll = Instant::now() + poll_interval;
                        if composed_changed(&config_path, &conf_d_dir, baseline) {
                            if try_restart(&config_path, &mut on_change) {
                                return; // one-shot
                            }
                            baseline = fresh_baseline(&config_path, &conf_d_dir);
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // Event channel gone mid-run; the poll carries on alone.
                    let Some(f) = on_change.take() else {
                        return;
                    };
                    poll_loop(&config_path, &conf_d_dir, poll_interval, baseline, f);
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
    conf_d_dir: &Path,
    poll_interval: Duration,
    start_baseline: Baseline,
    on_change: F,
) {
    let mut on_change = Some(on_change);
    let mut baseline = start_baseline;
    loop {
        std::thread::sleep(poll_interval);
        if composed_changed(config_path, conf_d_dir, baseline) {
            if try_restart(config_path, &mut on_change) {
                return;
            }
            baseline = fresh_baseline(config_path, conf_d_dir);
        }
    }
}

/// Recompute a `Baseline` from current disk state after a detected-but-not-
/// restarted change, so the same (possibly still-invalid) state isn't
/// re-flagged as "changed" on every subsequent tick.
fn fresh_baseline(config_path: &Path, conf_d_dir: &Path) -> Baseline {
    let (_, count) = composed_state(config_path, conf_d_dir);
    Baseline {
        since_ms: now_unix_ms(),
        count,
    }
}

/// Validate the composed config at `config_path` (its conf.d dir included)
/// and, if it parses cleanly, consume `on_change` and fire it (returns
/// `true` — caller must stop watching). If it fails to parse (including
/// "file doesn't exist", e.g. deleted, or any composed file being invalid),
/// logs a warning naming the error and returns `false` — caller keeps
/// watching.
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

/// Validates the WHOLE composed set (main file + every conf.d/*.toml) — see
/// `Config::parse_composed`. The main config file must still be readable;
/// that requirement is unchanged from before conf.d existed.
fn validate_config_file(config_path: &Path) -> Result<(), String> {
    let content = std::fs::read_to_string(config_path)
        .map_err(|e| format!("could not read config file: {e}"))?;
    let conf_d_dir = Config::conf_d_dir_for(config_path);
    Config::parse_composed(&content, &conf_d_dir).map(|_| ())
}

fn mtime_to_unix_ms(mtime: SystemTime) -> u64 {
    mtime
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
