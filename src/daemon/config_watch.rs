//! Config-file self-watch: mirrors the binary self-watch
//! (`crate::singleton::spawn_binary_self_watch_with`) — an fs-event watch as
//! the fast path, a guaranteed mtime poll as the backstop, 200ms debounce —
//! but gates the restart on the new file actually parsing. Config
//! self-supervision is deliberately not part of the singleton canon (that spec
//! covers binary freshness); the user-facing contract lives in the website
//! configuration reference.
//!
//! Not every change is a restart. A change that touches only what is under
//! `[daemon.providers]` or `[client.providers]` — Sources, `return` and
//! `template` lines, `cache` tables, `enabled`, a provider newly named or no
//! longer named — is reported as [`ChangeKind::Providers`] and the daemon
//! applies it in place: it builds the new registry (the load that gated the
//! edit), plans the reseed, swaps both in, brings the scheduler's lifecycle
//! entries to match and reseeds the template and `cache` facets. A change to a bare key of `[daemon]` or `[client]` (`socket_path`,
//! `log_level`, `plugin.path`, `budget`, `timeout`, the watchdog keys,
//! `idle_shutdown`) is [`ChangeKind::Settings`] and the daemon restarts,
//! because those are read once at startup. The classifier is the composed TOML
//! with both `providers` roots removed (`Config::settings`), compared
//! before and after. Because the providers case leaves the process running,
//! the watch re-arms after every call rather than firing once and exiting.
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
//! `docs/reference/configuration-reference.md` §"Composition / conf.d") and
//! to the managed `defaults/` directory (`Config::defaults_dir_for`, spec
//! §3.3): the composed set is every `defaults/*.toml` + the main config file +
//! every `conf.d/*.toml`. A change to any file in that set — the main file OR
//! a drop-in in either directory modified, added, or removed — runs through
//! the same debounce and parse-gate, validating the WHOLE composed set
//! (`Config::load_from`) before restarting. Because the two directories
//! are subdirectories, not siblings of the config file, each needs its own
//! fs-event registration (the parent-dir watch is `NonRecursive` and doesn't
//! see inside them) — registered separately, best-effort, when the directory
//! exists at watch-registration time or once it appears. The mtime-poll backstop holds a
//! snapshot of the whole composed set — one mtime per file — and calls any
//! difference a change, so an edit, an addition and a deletion are all noticed
//! (a deletion bumps no remaining file's mtime) and an edit to one file is
//! seen even while another carries a far-future mtime from clock skew.
//!
//! A drop-in directory that does not exist at registration time is watched
//! from the moment it appears: the parent-dir watch reports its creation,
//! which counts as a change (the files written into it arrive with it) and
//! registers the directory's own watch, so later edits inside it take the fast
//! path too. The config directory itself always exists by then: the daemon
//! writes `defaults/` into it before it loads the configuration.

use crate::config::Config;
use crate::provider::registry::{ConfigLoad, ProviderRegistry};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

/// Config self-watch poll interval — same 5s guarantee as the binary watch.
pub const CONFIG_WATCH_POLL_INTERVAL: Duration = crate::singleton::SELF_WATCH_POLL_INTERVAL;

/// What a configuration change turned out to be, decided by comparing the
/// composed *settings* — the deep-merged TOML with both `providers` roots
/// removed (`Config::settings`) — before and after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// The bare keys of `[daemon]` and `[client]` are as they were: only what
    /// is under `[daemon.providers]` or `[client.providers]` can differ. The
    /// daemon reloads in place and keeps serving.
    Providers,
    /// A bare key of `[daemon]` or `[client]` differs. The daemon restarts,
    /// because those are read once at startup.
    Settings,
}

/// The mtime of every file in the composed set that exists, keyed by path:
/// every `defaults/*.toml`, the main config file, and every `conf.d/*.toml`.
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
fn composed_snapshot(config_path: &Path, defaults_dir: &Path, conf_d_dir: &Path) -> Snapshot {
    let mut out = Snapshot::new();
    let mut note = |p: PathBuf| {
        if let Ok(meta) = std::fs::metadata(&p) {
            let ms = meta.modified().map(mtime_to_unix_ms).unwrap_or(0);
            out.insert(p, ms);
        }
    };
    for f in Config::conf_d_files(defaults_dir) {
        note(f);
    }
    note(config_path.to_path_buf());
    for f in Config::conf_d_files(conf_d_dir) {
        note(f);
    }
    out
}

/// Spawn a thread watching the files `running` was loaded from: its
/// `config.toml`, `defaults/` and `conf.d`. `running` is the configuration
/// the daemon runs on, and what the first change is judged against. Each time
/// the composed set changes and parses as valid config, calls `on_change` with
/// the kind of change and what the change loaded (after a 200ms debounce on the
/// fs-event path) and **keeps watching** — a providers change is handled in place, so there is no process
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
/// `None` for a configuration with no file behind it, which has nothing to
/// watch.
pub fn spawn_config_self_watch_with<F: FnMut(ChangeKind, ConfigLoad) + Send + 'static>(
    running: &Config,
    poll_interval: Duration,
    fs_events: bool,
    stop: CancellationToken,
    mut on_change: F,
) -> Option<std::thread::JoinHandle<()>> {
    use notify::{EventKind, RecursiveMode, Watcher};
    use std::sync::mpsc;

    // The starting point is read on the caller's thread, not the watch's: a
    // change landing between the spawn and the thread's first instruction would
    // otherwise become the "before" state and classify itself as no change.
    let config_path = running.config_file()?.to_path_buf();
    let defaults_dir = Config::defaults_dir_for(&config_path);
    let conf_d_dir = Config::conf_d_dir_for(&config_path);
    let mut snapshot = composed_snapshot(&config_path, &defaults_dir, &conf_d_dir);
    let mut settings = running.settings();

    Some(std::thread::spawn(move || {
        let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
        let watcher = if fs_events {
            match (notify::recommended_watcher(tx), config_path.parent()) {
                (Ok(mut w), Some(parent)) => match w.watch(parent, RecursiveMode::NonRecursive) {
                    Ok(()) => {
                        tracing::debug!(
                            "config-watch: watching {parent:?} for changes to {config_path:?}"
                        );
                        // defaults/ and conf.d are subdirectories, not siblings —
                        // the parent-dir watch above is NonRecursive and can't
                        // see inside them, so each needs its own registration.
                        // Best-effort: only possible if the directory already
                        // exists, and its absence doesn't invalidate the rest
                        // of the watch (the poll backstop covers both either
                        // way).
                        for dir in [&defaults_dir, &conf_d_dir] {
                            watch_dir(&mut w, dir);
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
                &defaults_dir,
                &conf_d_dir,
                poll_interval,
                &stop,
                &mut snapshot,
                &mut settings,
                &mut on_change,
            );
            return;
        }
        let mut watcher = watcher;

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
                    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
                    let target = canonical(&config_path);
                    let dirs = [defaults_dir.as_path(), conf_d_dir.as_path()];
                    let dir_targets = dirs.map(canonical);
                    let dir_targets = [dir_targets[0].as_path(), dir_targets[1].as_path()];
                    let path_match = event.paths.iter().any(|p| {
                        touches_composed_set(p, &config_path, &dirs)
                            || touches_composed_set(&canonical(p), &target, &dir_targets)
                    });
                    // A directory of the set that has just appeared (or come
                    // back) needs its own registration to see inside it.
                    if let (Some(w), EventKind::Create(_)) = (watcher.as_mut(), event.kind) {
                        for (dir, dir_target) in dirs.into_iter().zip(dir_targets) {
                            if event
                                .paths
                                .iter()
                                .any(|p| p == dir || canonical(p) == dir_target)
                            {
                                watch_dir(w, dir);
                            }
                        }
                    }
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
                        snapshot = composed_snapshot(&config_path, &defaults_dir, &conf_d_dir);
                        try_fire(&config_path, &mut settings, &mut on_change);
                    }
                    if Instant::now() >= next_poll {
                        next_poll = Instant::now() + poll_interval;
                        check_snapshot(
                            &config_path,
                            &defaults_dir,
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
                        &defaults_dir,
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
    }))
}

/// Register a non-recursive watch on `dir` if it exists. Best-effort: a
/// directory that is absent, or cannot be watched, leaves its changes to the
/// poll backstop.
fn watch_dir(w: &mut impl notify::Watcher, dir: &Path) {
    if !dir.is_dir() {
        return;
    }
    match w.watch(dir, notify::RecursiveMode::NonRecursive) {
        Ok(()) => tracing::debug!("config-watch: watching dir {dir:?}"),
        Err(e) => tracing::warn!(
            "config-watch: failed to watch dir {dir:?}: {e}; its changes rely on the poll backstop"
        ),
    }
}

/// Whether an fs event at `p` touches the composed set whose main file is
/// `file` and whose drop-in directories are `dirs`: the main file, a `*.toml`
/// directly in one of `dirs`, or one of `dirs` itself appearing or going (the
/// parent-dir watch is the only one that sees that, and the files inside it
/// arrive with it).
fn touches_composed_set(p: &Path, file: &Path, dirs: &[&Path]) -> bool {
    let is_toml = p.extension().and_then(|e| e.to_str()) == Some("toml");
    p == file
        || dirs
            .iter()
            .any(|d| p == *d || (is_toml && p.parent() == Some(*d)))
}

/// Poll fallback used both when the fs-event watcher couldn't be set up at
/// all, and when its channel disconnects mid-run. Each change that validates
/// cleanly is reported and the watch re-arms; an invalid change is logged and
/// polling continues. Ends when `stop` is cancelled.
#[allow(clippy::too_many_arguments)]
fn poll_loop<F: FnMut(ChangeKind, ConfigLoad)>(
    config_path: &Path,
    defaults_dir: &Path,
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
        check_snapshot(
            config_path,
            defaults_dir,
            conf_d_dir,
            snapshot,
            settings,
            on_change,
        );
    }
}

/// Compare the composed set against `snapshot` and, if it moved, adopt what was
/// observed and put the change through the parse gate.
///
/// The snapshot is replaced *before* firing, not after, for two reasons: a
/// change that fails the parse gate must not be re-detected on every subsequent
/// tick, and a further edit landing while the gate runs must still differ from
/// the snapshot so it fires on the next one.
fn check_snapshot<F: FnMut(ChangeKind, ConfigLoad)>(
    config_path: &Path,
    defaults_dir: &Path,
    conf_d_dir: &Path,
    snapshot: &mut Snapshot,
    settings: &mut toml::Value,
    on_change: &mut F,
) {
    let observed = composed_snapshot(config_path, defaults_dir, conf_d_dir);
    if observed == *snapshot {
        return;
    }
    *snapshot = observed;
    try_fire(config_path, settings, on_change);
}

/// Load the composed config at `config_path` (its defaults and conf.d dirs
/// included) through the one loader startup uses and, if it loads without an
/// error, classify the change against `settings` and fire `on_change`,
/// leaving `settings` holding the new snapshot. A missing main config file is
/// nothing, not a failure. If any composed file cannot be read or parsed, or
/// breaks a rule, logs the errors and fires nothing.
fn try_fire<F: FnMut(ChangeKind, ConfigLoad)>(
    config_path: &Path,
    settings: &mut toml::Value,
    on_change: &mut F,
) {
    // The checks startup runs, so a config the daemon would refuse to start on
    // is never restarted into. The running daemon keeps its configuration.
    let config = Config::load_from(config_path);
    let vars = std::env::vars().collect();
    let loaded = ProviderRegistry::load(&config, &vars);
    for w in &loaded.warnings {
        tracing::warn!("{w}");
    }
    if !loaded.errors.is_empty() {
        for e in &loaded.errors {
            tracing::error!("{e}");
        }
        tracing::warn!(
            "config file changed but does not load ({} error(s)); keeping current config, \
             not restarting",
            loaded.errors.len()
        );
        return;
    }
    let next = config.settings();
    let kind = if next == *settings {
        ChangeKind::Providers
    } else {
        ChangeKind::Settings
    };
    *settings = next;
    tracing::info!("config file changed and parses cleanly ({kind:?})");
    on_change(kind, loaded);
}

fn mtime_to_unix_ms(mtime: SystemTime) -> u64 {
    mtime
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "[daemon.providers.demo.sources.s]\ndata = \"{{ 1 }}\"\n\
                          result = \"{{ data }}\"\nreturn = \"{{ result | poll('5s') }}\"\n";

    /// A config directory holding `main` as `config.toml`.
    struct Dir {
        _tmp: tempfile::TempDir,
        config: PathBuf,
        settings: toml::Value,
    }

    impl Dir {
        fn new(main: &str) -> Dir {
            let tmp = tempfile::tempdir().unwrap();
            let config = tmp.path().join("config.toml");
            std::fs::write(&config, main).unwrap();
            std::fs::create_dir(tmp.path().join("conf.d")).unwrap();
            let mut dir = Dir {
                _tmp: tmp,
                config,
                settings: toml::Value::Table(Default::default()),
            };
            dir.settings = Config::load_from(&dir.config).settings();
            dir
        }

        fn write(&self, rel: &str, body: &str) {
            std::fs::write(self.config.parent().unwrap().join(rel), body).unwrap();
        }

        /// What `try_fire` reports for the files as they are now.
        fn fire(&mut self) -> Vec<ChangeKind> {
            let mut got = Vec::new();
            try_fire(&self.config, &mut self.settings, &mut |kind, _| {
                got.push(kind)
            });
            got
        }
    }

    /// `conf.d/` created after the watch began is reported by the parent-dir
    /// watch as the directory itself, not as a file in it: that is a change,
    /// or the drop-ins written into it wait for the poll backstop.
    #[test]
    fn a_drop_in_directory_appearing_touches_the_composed_set() {
        let file = Path::new("/c/config.toml");
        let dirs = [Path::new("/c/defaults"), Path::new("/c/conf.d")];
        let touches = |p: &str| touches_composed_set(Path::new(p), file, &dirs);
        assert!(touches("/c/conf.d"), "conf.d itself appearing");
        assert!(touches("/c/defaults"), "defaults/ itself appearing");
        assert!(touches("/c/conf.d/50-mine.toml"));
        assert!(touches("/c/config.toml"));
        assert!(!touches("/c/conf.d/notes.txt"), "not a drop-in");
        assert!(!touches("/c/state"), "a sibling the set does not include");
        assert!(
            !touches("/c/conf.d/sub/10-x.toml"),
            "not directly in conf.d"
        );
    }

    #[test]
    fn a_bad_return_in_conf_d_reports_nothing_and_fixing_it_reports_a_reload() {
        let mut dir = Dir::new(SOURCE);
        dir.write(
            "conf.d/10-mine.toml",
            "[daemon.providers.demo.sources.s]\nreturn = \"{{ result | poll('banana') }}\"\n",
        );
        assert_eq!(
            dir.fire(),
            vec![],
            "a return that does not load is not a restart"
        );

        dir.write(
            "conf.d/10-mine.toml",
            "[daemon.providers.demo.sources.s]\nreturn = \"{{ result | poll('9s') }}\"\n",
        );
        assert_eq!(dir.fire(), vec![ChangeKind::Providers]);
    }

    #[test]
    fn a_toml_syntax_error_reports_nothing_and_fixing_it_reports_a_restart() {
        let mut dir = Dir::new(SOURCE);
        dir.write("conf.d/10-mine.toml", "[daemon]\nsocket_path = \n");
        assert_eq!(
            dir.fire(),
            vec![],
            "a file that does not parse is not a restart"
        );
        dir.write("conf.d/10-mine.toml", "[daemon]\nlog_level = \"debug\"\n");
        assert_eq!(dir.fire(), vec![ChangeKind::Settings]);
    }

    /// The load handed to the callback is the one the gate validated, its
    /// seed planned with it, so what is reseeded is what was checked.
    #[test]
    fn the_callback_gets_the_load_that_was_validated() {
        let mut dir = Dir::new(SOURCE);
        dir.write(
            "conf.d/10-t.toml",
            "[client.providers.x]\ntemplate.a = \"1\"\n",
        );
        let mut seen = Vec::new();
        try_fire(&dir.config, &mut dir.settings, &mut |kind, loaded| {
            let labels: Vec<String> = loaded.seed.writes["x:template"]
                .iter()
                .map(|(label, _)| label.clone())
                .collect();
            seen.push((kind, loaded.errors.len(), labels));
        });
        assert_eq!(seen.len(), 1);
        let (kind, errors, labels) = &seen[0];
        assert_eq!((*kind, *errors), (ChangeKind::Providers, 0));
        assert!(
            matches!(labels.as_slice(), [label] if label.ends_with("conf.d/10-t.toml")),
            "the drop-in's template, seeded from the drop-in: {labels:?}"
        );
    }

    #[test]
    fn an_enabled_edit_is_a_providers_change() {
        let mut dir = Dir::new(SOURCE);
        dir.write(
            "conf.d/10-off.toml",
            "[daemon.providers.git]\nenabled = false\n",
        );
        assert_eq!(dir.fire(), vec![ChangeKind::Providers]);
    }

    #[test]
    fn a_source_or_return_edit_is_a_providers_change() {
        let mut dir = Dir::new(SOURCE);
        dir.write(
            "conf.d/10-mine.toml",
            "[daemon.providers.demo.sources.s]\nreturn = \"{{ result | poll('9s') }}\"\n",
        );
        assert_eq!(dir.fire(), vec![ChangeKind::Providers]);
    }

    #[test]
    fn a_bare_daemon_key_is_a_settings_change() {
        let mut dir = Dir::new(SOURCE);
        dir.write("conf.d/10-c.toml", "[daemon]\nidle_shutdown = \"10m\"\n");
        assert_eq!(dir.fire(), vec![ChangeKind::Settings]);
    }

    #[test]
    fn a_budget_edit_is_a_settings_change() {
        let mut dir = Dir::new(SOURCE);
        dir.write("conf.d/10-budget.toml", "[daemon]\nbudget = \"20ms\"\n");
        assert_eq!(dir.fire(), vec![ChangeKind::Settings]);
    }

    #[test]
    fn a_per_file_error_in_a_source_table_reports_nothing() {
        let mut dir = Dir::new(SOURCE);
        dir.write("config.toml", &format!("{SOURCE}command = \"echo\"\n"));
        assert_eq!(dir.fire(), vec![]);
    }

    #[test]
    fn a_name_collision_with_a_loaded_provider_reports_nothing() {
        let mut dir = Dir::new(SOURCE);
        dir.write(
            "conf.d/10-mine.toml",
            "[daemon.providers.git.sources.mine]\ndata = \"{{ 1 }}\"\n\
             result.branch = \"{{ data }}\"\n",
        );
        assert_eq!(dir.fire(), vec![], "git declares `branch` already");
    }

    #[test]
    fn a_plugin_path_edit_is_a_settings_change() {
        let mut dir = Dir::new(SOURCE);
        dir.write(
            "conf.d/10-plugins.toml",
            "[daemon.plugin]\npath = [\"/opt/plugins\"]\n",
        );
        assert_eq!(dir.fire(), vec![ChangeKind::Settings]);
    }

    #[test]
    fn a_valid_edit_reports_its_kind() {
        let mut dir = Dir::new(SOURCE);
        dir.write(
            "conf.d/10-t.toml",
            "[client.providers.x]\ntemplate.a = \"1\"\n",
        );
        assert_eq!(dir.fire(), vec![ChangeKind::Providers]);
    }
}
