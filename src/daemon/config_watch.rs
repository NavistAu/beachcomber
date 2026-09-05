//! Config-file self-watch: mirrors the binary self-watch
//! (`crate::singleton::spawn_binary_self_watch_with`) — an fs-event watch as
//! the fast path, a guaranteed mtime poll as the backstop, 200ms debounce —
//! but gates the restart on the new file actually parsing. Config
//! self-supervision is deliberately not part of the singleton canon (that spec
//! covers binary freshness); the user-facing contract lives in the website
//! configuration reference.
//!
//! Not every change is a restart. A change that touches only
//! `[providers."<p>:<facet>"]` tables is reported as [`ChangeKind::FacetsOnly`]
//! and the daemon re-seeds them in place; anything else is
//! [`ChangeKind::Settings`] and the daemon restarts, because settings are read
//! once at startup. The classifier is the composed TOML with the facet tables
//! removed (`Config::settings_only`), compared before and after. Because the
//! facet case leaves the process running, the watch re-arms after every call
//! rather than firing once and exiting.
//!
//! The watch targets the config path its caller passes — the directory the
//! running `Config` was loaded from (`Config::config_dir`), never a path
//! re-resolved from the ambient environment — and is not gated on the file
//! existing at daemon startup. Because the watch is against a *path* rather
//! than an open file, a config file that doesn't exist yet when the daemon
//! starts is picked up naturally once it's created — the mtime poll's `stat`
//! starts succeeding once the file exists, and the fs-event watch (registered
//! on the parent directory, same as the binary watch) delivers a Create event
//! the same way it would for a binary replacement.
//!
//! The thread exits when the `stop` token its caller passes is cancelled. The
//! daemon passes its own shutdown token, so the watch dies with the daemon
//! instead of outliving it holding an `Arc<Cache>` — which for an in-process
//! daemon (a test, an embedder) would be a leak rather than a process exit.
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
//! directory exists at watch-registration time. The mtime-poll backstop holds
//! a snapshot of the whole composed set — one mtime per file — and calls any
//! difference a change, so an edit, an addition and a deletion are all noticed
//! (a deletion bumps no remaining file's mtime) and an edit to one file is
//! seen even while another carries a far-future mtime from clock skew.
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
use tokio_util::sync::CancellationToken;

/// Config self-watch poll interval — same 5s guarantee as the binary watch.
pub const CONFIG_WATCH_POLL_INTERVAL: Duration = crate::singleton::SELF_WATCH_POLL_INTERVAL;

/// What a configuration change turned out to be, decided by comparing the
/// composed *settings* — the deep-merged TOML with every facet table removed
/// (`Config::settings_only`) — before and after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// Only `[providers."<p>:<facet>"]` tables differ. The daemon re-seeds them
    /// in place and keeps serving.
    FacetsOnly,
    /// Something outside the facet tables differs. The daemon restarts, because
    /// settings are read once at startup.
    Settings,
}

/// The mtime of every file in the composed set that exists, keyed by path:
/// the main config file plus every `conf.d/*.toml`.
///
/// Per file rather than "newest mtime plus a count", which two things defeat.
/// A file carrying a far-future mtime — clock skew across a network mount, an
/// editor writing one, a test stamping one to beat filesystem granularity —
/// stays newer than every later reference point, so with one timestamp an edit
/// to any *other* file is invisible behind it and the watch either misses the
/// change or fires on every tick forever. A map has neither failure: a change
/// is any path whose mtime moved (in either direction) plus any path added or
/// removed, and after each one the snapshot becomes the state just observed,
/// so an unchanged set compares equal whatever the timestamps say.
type Snapshot = std::collections::BTreeMap<PathBuf, u64>;

/// Read the composed set's current [`Snapshot`]. A file whose mtime cannot be
/// read is recorded as 0 rather than left out, so it does not read as a
/// deletion on every tick.
fn composed_snapshot(config_path: &Path, conf_d_dir: &Path) -> Snapshot {
    let mut out = Snapshot::new();
    let mut note = |p: PathBuf| {
        if let Ok(meta) = std::fs::metadata(&p) {
            let ms = meta.modified().map(mtime_to_unix_ms).unwrap_or(0);
            out.insert(p, ms);
        }
    };
    note(config_path.to_path_buf());
    for f in Config::conf_d_files(conf_d_dir) {
        note(f);
    }
    out
}

/// Spawn a thread watching `config_path` and its `conf.d`. Each time the
/// composed set changes and parses as valid config, calls `on_change` with the
/// kind of change (after a 200ms debounce on the fs-event path) and **keeps
/// watching** — a facet-only change is handled in place, so there is no process
/// exit to end the watch. A change that fails to parse logs a warning naming the
/// error and changes nothing: the daemon never restarts into a config that would
/// fail startup.
///
/// `poll_interval` and `fs_events` are the two knobs the caller picks:
/// production takes [`CONFIG_WATCH_POLL_INTERVAL`] with the fs-event fast path
/// on, and in-process daemons take a short interval with `fs_events: false` —
/// the degraded mode the mtime poll exists for, and the mode the tests drive.
///
/// Cancelling `stop` ends the thread, at the latest one `poll_interval` later.
/// Returns its `JoinHandle` so a caller that needs to know it has gone can wait
/// for it; the daemon does not, because its own exit is what cancels the token.
pub fn spawn_config_self_watch_with<F: FnMut(ChangeKind) + Send + 'static>(
    config_path: PathBuf,
    poll_interval: Duration,
    fs_events: bool,
    stop: CancellationToken,
    mut on_change: F,
) -> std::thread::JoinHandle<()> {
    use notify::{EventKind, RecursiveMode, Watcher};
    use std::sync::mpsc;

    // The starting point is read on the caller's thread, not the watch's: a
    // change landing between the spawn and the thread's first instruction would
    // otherwise become the "before" state and classify itself as no change.
    let conf_d_dir = Config::conf_d_dir_for(&config_path);
    let mut snapshot = composed_snapshot(&config_path, &conf_d_dir);
    let mut settings = settings_snapshot(&config_path, &conf_d_dir);

    std::thread::spawn(move || {
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
                &stop,
                &mut snapshot,
                &mut settings,
                &mut on_change,
            );
            return;
        }
        let _watcher = watcher;

        let debounce = Duration::from_millis(200);
        let mut last_event: Option<Instant> = None;
        let mut next_poll = Instant::now() + poll_interval;

        loop {
            if stop.is_cancelled() {
                tracing::debug!("config-watch: stopped");
                return;
            }
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
                        snapshot = composed_snapshot(&config_path, &conf_d_dir);
                        try_fire(&config_path, &conf_d_dir, &mut settings, &mut on_change);
                    }
                    if Instant::now() >= next_poll {
                        next_poll = Instant::now() + poll_interval;
                        check_snapshot(
                            &config_path,
                            &conf_d_dir,
                            &mut snapshot,
                            &mut settings,
                            &mut on_change,
                        );
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // Event channel gone mid-run; the poll carries on alone.
                    poll_loop(
                        &config_path,
                        &conf_d_dir,
                        poll_interval,
                        &stop,
                        &mut snapshot,
                        &mut settings,
                        &mut on_change,
                    );
                    return;
                }
            }
        }
    })
}

/// Poll fallback used both when the fs-event watcher couldn't be set up at
/// all, and when its channel disconnects mid-run. Each change that validates
/// cleanly is reported and the watch re-arms; an invalid change is logged and
/// polling continues. Ends when `stop` is cancelled.
#[allow(clippy::too_many_arguments)]
fn poll_loop<F: FnMut(ChangeKind)>(
    config_path: &Path,
    conf_d_dir: &Path,
    poll_interval: Duration,
    stop: &CancellationToken,
    snapshot: &mut Snapshot,
    settings: &mut toml::Value,
    on_change: &mut F,
) {
    loop {
        std::thread::sleep(poll_interval);
        if stop.is_cancelled() {
            tracing::debug!("config-watch: stopped");
            return;
        }
        check_snapshot(config_path, conf_d_dir, snapshot, settings, on_change);
    }
}

/// Compare the composed set against `snapshot` and, if it moved, adopt what was
/// observed and put the change through the parse gate.
///
/// The snapshot is replaced *before* firing, not after, for two reasons: a
/// change that fails the parse gate must not be re-detected on every subsequent
/// tick, and a further edit landing while the gate runs must still differ from
/// the snapshot so it fires on the next one.
fn check_snapshot<F: FnMut(ChangeKind)>(
    config_path: &Path,
    conf_d_dir: &Path,
    snapshot: &mut Snapshot,
    settings: &mut toml::Value,
    on_change: &mut F,
) {
    let observed = composed_snapshot(config_path, conf_d_dir);
    if observed == *snapshot {
        return;
    }
    *snapshot = observed;
    try_fire(config_path, conf_d_dir, settings, on_change);
}

/// Validate the composed config at `config_path` (its conf.d dir included)
/// and, if it parses cleanly, classify the change against `settings` and fire
/// `on_change`, leaving `settings` holding the new snapshot. A missing main
/// config file parses as empty content (see `validate_config_file`), not a
/// failure. If the composed set fails to parse (any composed file being
/// invalid, or the main file unreadable for a reason other than not existing),
/// logs a warning naming the error and fires nothing.
fn try_fire<F: FnMut(ChangeKind)>(
    config_path: &Path,
    conf_d_dir: &Path,
    settings: &mut toml::Value,
    on_change: &mut F,
) {
    let result = validate_config_file(config_path);
    if !should_restart_for_config_change(&result) {
        let e = result.unwrap_err();
        tracing::warn!(
            "config file changed but is not valid ({e}); keeping current config, not restarting"
        );
        return;
    }
    let next = settings_snapshot(config_path, conf_d_dir);
    let kind = if next == *settings {
        ChangeKind::FacetsOnly
    } else {
        ChangeKind::Settings
    };
    *settings = next;
    tracing::info!("config file changed and parses cleanly ({kind:?})");
    on_change(kind);
}

/// The composed configuration with every facet table removed — the half a
/// change has to touch for a restart to be warranted.
fn settings_snapshot(config_path: &Path, conf_d_dir: &Path) -> toml::Value {
    let mut composed = toml::Value::Table(Default::default());
    for (_, layer) in Config::layers(Some(config_path), conf_d_dir) {
        crate::config::deep_merge_toml(&mut composed, layer);
    }
    Config::settings_only(composed)
}

/// Validates the WHOLE composed set (main file + every conf.d/*.toml) — see
/// `Config::parse_composed`. A missing main config file is treated as empty
/// content, same tolerance `Config::load()` already gives daemon startup (a
/// config that lives entirely in conf.d/ is legitimate); any other read
/// error (e.g. permissions) still fails validation.
fn validate_config_file(config_path: &Path) -> Result<(), String> {
    let content = match std::fs::read_to_string(config_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("could not read config file: {e}")),
    };
    let conf_d_dir = Config::conf_d_dir_for(config_path);
    Config::parse_composed(&content, &conf_d_dir).map(|_| ())
}

fn mtime_to_unix_ms(mtime: SystemTime) -> u64 {
    mtime
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
