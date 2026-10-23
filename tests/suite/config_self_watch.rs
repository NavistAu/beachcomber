//! Integration tests for the daemon's config-file self-watch
//! (`src/daemon/config_watch.rs`), which mirrors the binary self-watch
//! (`tests/suite/binary_self_watch.rs`) but only restarts when the changed file
//! parses cleanly.
//!
//! These use the polling backend (`fs_events: false`) so they run reliably
//! under sandboxed hosts that can't use FSEvents / inotify, same convention
//! as the watcher tests in `tests/`.

use crate::common::mtime::touch_future;
use beachcomber::daemon::config_watch::{ChangeKind, spawn_config_self_watch_with};
use std::path::PathBuf;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Spawn a watch over `cfg` polling every `ms` milliseconds with the fs-event
/// fast path off, and hand back its thread plus a channel of the changes it
/// reports. Every test here wants exactly this; only the two that pin the stop
/// token or the change kind look past the receiver.
fn watch(
    cfg: PathBuf,
    ms: u64,
) -> (
    std::thread::JoinHandle<()>,
    std::sync::mpsc::Receiver<ChangeKind>,
) {
    watch_until(cfg, ms, CancellationToken::new())
}

/// [`watch`] with a caller-owned stop token.
fn watch_until(
    cfg: PathBuf,
    ms: u64,
    stop: CancellationToken,
) -> (
    std::thread::JoinHandle<()>,
    std::sync::mpsc::Receiver<ChangeKind>,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = spawn_config_self_watch_with(
        cfg,
        Duration::from_millis(ms),
        false, // fs events off — the mtime poll is the mechanism under test
        stop,
        move |kind, _| {
            let _ = tx.send(kind);
        },
    );
    (handle, rx)
}

#[test]
fn poll_restarts_on_valid_config_change_when_fs_events_are_dead() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "[daemon]\n").unwrap();

    let (_watch, rx) = watch(cfg.clone(), 100);

    std::fs::write(&cfg, "[daemon]\nlog_level = \"debug\"\n").unwrap();
    touch_future(&cfg);

    rx.recv_timeout(Duration::from_secs(3))
        .expect("valid config change triggered restart");
}

#[test]
fn poll_ignores_invalid_config_change_and_keeps_running() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "[daemon]\n").unwrap();

    let (_watch, rx) = watch(cfg.clone(), 100);

    // Not valid TOML.
    std::fs::write(&cfg, "this is not [ valid toml").unwrap();
    touch_future(&cfg);

    // Give the poll several intervals to notice and (wrongly) fire; it must not.
    let result = rx.recv_timeout(Duration::from_millis(800));
    assert!(
        result.is_err(),
        "daemon must not restart into a config that fails to parse"
    );
}

#[test]
fn config_file_created_after_daemon_start_is_picked_up() {
    // Spec item: "the daemon may have started with no config file — handle
    // the file appearing later if the existing watch machinery makes that
    // natural". Here it's natural: the poll stats the path regardless of
    // whether anything exists there yet.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml"); // does not exist yet

    let (_watch, rx) = watch(cfg.clone(), 100);

    std::fs::write(&cfg, "[daemon]\n").unwrap();
    touch_future(&cfg);

    rx.recv_timeout(Duration::from_secs(3))
        .expect("config file created after startup should be picked up");
}

#[test]
fn config_file_that_never_appears_never_restarts() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("never-written.toml");

    let (_watch, rx) = watch(cfg.clone(), 100);

    let result = rx.recv_timeout(Duration::from_millis(500));
    assert!(
        result.is_err(),
        "no config file ever appeared; daemon must not restart"
    );
}

// ── conf.d coverage ──────────────────────────────────────────────────────

#[test]
fn conf_d_file_change_triggers_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "[daemon]\n").unwrap();
    let conf_d = tmp.path().join("conf.d");
    std::fs::create_dir(&conf_d).unwrap();
    let drop_in = conf_d.join("01-drop-in.toml");
    std::fs::write(&drop_in, "[daemon]\nlog_level = \"debug\"\n").unwrap();

    let (_watch, rx) = watch(cfg.clone(), 100);

    std::fs::write(&drop_in, "[daemon]\nlog_level = \"trace\"\n").unwrap();
    touch_future(&drop_in);

    rx.recv_timeout(Duration::from_secs(3))
        .expect("conf.d drop-in change should trigger restart");
}

#[test]
fn conf_d_invalid_edit_does_not_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "[daemon]\n").unwrap();
    let conf_d = tmp.path().join("conf.d");
    std::fs::create_dir(&conf_d).unwrap();
    let drop_in = conf_d.join("01-drop-in.toml");
    std::fs::write(&drop_in, "[daemon]\nlog_level = \"debug\"\n").unwrap();

    let (_watch, rx) = watch(cfg.clone(), 100);

    std::fs::write(&drop_in, "this is not [ valid toml").unwrap();
    touch_future(&drop_in);

    let result = rx.recv_timeout(Duration::from_millis(800));
    assert!(
        result.is_err(),
        "daemon must not restart when a conf.d drop-in fails to parse"
    );
}

#[test]
fn conf_d_only_change_restarts_without_a_main_config_file() {
    // A daemon whose config lives entirely in conf.d/ (no config.toml at
    // all) must still restart on a conf.d change. Validation must treat a
    // missing main config file as empty content — same tolerance `Config::load()`
    // already gives daemon startup — not as a failure that blocks every restart.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml"); // deliberately never created
    let conf_d = tmp.path().join("conf.d");
    std::fs::create_dir(&conf_d).unwrap();
    let drop_in = conf_d.join("01-drop-in.toml");
    std::fs::write(&drop_in, "[daemon]\nlog_level = \"debug\"\n").unwrap();

    let (_watch, rx) = watch(cfg.clone(), 100);

    std::fs::write(&drop_in, "[daemon]\nlog_level = \"trace\"\n").unwrap();
    touch_future(&drop_in);

    rx.recv_timeout(Duration::from_secs(3))
        .expect("conf.d change should trigger restart even when config.toml doesn't exist");
}

#[test]
fn new_file_appearing_in_conf_d_triggers_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "[daemon]\n").unwrap();
    let conf_d = tmp.path().join("conf.d");
    std::fs::create_dir(&conf_d).unwrap();

    let (_watch, rx) = watch(cfg.clone(), 100);

    let new_file = conf_d.join("02-new.toml");
    std::fs::write(&new_file, "[daemon]\nlog_level = \"warn\"\n").unwrap();
    touch_future(&new_file);

    rx.recv_timeout(Duration::from_secs(3))
        .expect("a new conf.d file should trigger restart (file-count delta)");
}

// ── re-arming and change classification ──────────────────────────────────────

#[test]
fn the_watch_re_arms_and_reports_every_change() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "[daemon]\n").unwrap();

    let (_watch, rx) = watch(cfg.clone(), 50);

    std::fs::write(&cfg, "[daemon]\nlog_level = \"debug\"\n").unwrap();
    touch_future(&cfg);
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(3)),
        Ok(ChangeKind::Settings),
        "first change"
    );

    std::fs::write(&cfg, "[daemon]\nlog_level = \"trace\"\n").unwrap();
    touch_future(&cfg);
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(3)),
        Ok(ChangeKind::Settings),
        "the watch re-arms: a second change is reported too"
    );
}

#[test]
fn a_facet_only_edit_classifies_as_facets_only() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(
        &cfg,
        "[daemon]\nlog_level = \"debug\"\n\n[client.providers.x]\ntemplate.a = \"1\"\n",
    )
    .unwrap();

    let (_watch, rx) = watch(cfg.clone(), 50);

    std::fs::write(
        &cfg,
        "[daemon]\nlog_level = \"debug\"\n\n[client.providers.x]\ntemplate.a = \"2\"\n",
    )
    .unwrap();
    touch_future(&cfg);

    assert_eq!(
        rx.recv_timeout(Duration::from_secs(3)),
        Ok(ChangeKind::FacetsOnly),
        "only a client template changed — the daemon re-seeds instead of restarting"
    );
}

#[test]
fn a_decay_table_edit_classifies_as_facets_only() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(
        &cfg,
        "[daemon]\nlog_level = \"debug\"\n\n[daemon.providers.git.cache._]\ndecay = 2\n",
    )
    .unwrap();

    let (_watch, rx) = watch(cfg.clone(), 50);

    // The edit changes a decay and adds a provider whose only table is its
    // cache: neither is a setting the daemon reads once at startup.
    std::fs::write(
        &cfg,
        "[daemon]\nlog_level = \"debug\"\n\n[daemon.providers.git.cache._]\ndecay = 3\n\n\
         [daemon.providers.other.cache._]\ndecay = 1\n",
    )
    .unwrap();
    touch_future(&cfg);

    assert_eq!(
        rx.recv_timeout(Duration::from_secs(3)),
        Ok(ChangeKind::FacetsOnly),
        "only daemon.providers.<p>.cache tables changed — re-seed, not restart"
    );
}

#[test]
fn an_edit_is_seen_behind_another_file_stamped_into_the_future() {
    // One "newest mtime" reference point cannot see past a file whose mtime is
    // far ahead — a network mount's clock skew, an editor, a test. Per-file
    // mtimes can: only the file that actually moved compares unequal.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "[daemon]\n").unwrap();
    let conf_d = tmp.path().join("conf.d");
    std::fs::create_dir(&conf_d).unwrap();

    let ahead = conf_d.join("01-ahead.toml");
    std::fs::write(&ahead, "[daemon]\n").unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&ahead)
        .unwrap()
        .set_modified(std::time::SystemTime::now() + Duration::from_secs(3600))
        .unwrap();

    let other = conf_d.join("02-other.toml");
    std::fs::write(&other, "[daemon]\nlog_level = \"debug\"\n").unwrap();

    let (_watch, rx) = watch(cfg.clone(), 50);

    std::fs::write(&other, "[daemon]\nlog_level = \"trace\"\n").unwrap();
    touch_future(&other);

    assert_eq!(
        rx.recv_timeout(Duration::from_secs(3)),
        Ok(ChangeKind::Settings),
        "an edit to one file must be seen even while another is stamped an hour ahead"
    );
    assert!(
        rx.recv_timeout(Duration::from_millis(400)).is_err(),
        "and the file stamped ahead must not re-trigger on every tick"
    );
}

#[test]
fn cancelling_the_stop_token_ends_the_watch_thread() {
    // The watch's closure holds the daemon's cache and registry, so a watch
    // that outlives its daemon is a leak rather than a process exit.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "[daemon]\n").unwrap();

    let stop = CancellationToken::new();
    let (thread, rx) = watch_until(cfg.clone(), 50, stop.clone());

    // Alive and reporting first, so the silence below means something.
    std::fs::write(&cfg, "[daemon]\nlog_level = \"debug\"\n").unwrap();
    touch_future(&cfg);
    rx.recv_timeout(Duration::from_secs(3))
        .expect("the watch reports while it is running");

    stop.cancel();

    // `JoinHandle::join` has no timeout of its own; bound it through a channel
    // so a watch that never exits fails the test instead of hanging it.
    let (joined_tx, joined_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = thread.join();
        let _ = joined_tx.send(());
    });
    joined_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("the watch thread exits once its stop token is cancelled");

    std::fs::write(&cfg, "[daemon]\nlog_level = \"trace\"\n").unwrap();
    touch_future(&cfg);
    assert!(
        rx.recv_timeout(Duration::from_millis(400)).is_err(),
        "a stopped watch must report nothing"
    );
}

// ── a Source that would not load ────────────────────────────────────────────

/// The gate that keeps the daemon up on a bad edit is unit-tested in
/// `config_watch.rs` (`try_fire` is synchronous); this is the positive event,
/// through the real watch: a Source file that loads restarts.
#[test]
fn a_fixed_return_in_conf_d_restarts() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(
        &cfg,
        "[daemon.providers.demo.sources.s]\ndata = \"{{ 1 }}\"\nresult = \"{{ data }}\"\n",
    )
    .unwrap();
    let conf_d = tmp.path().join("conf.d");
    std::fs::create_dir_all(&conf_d).unwrap();
    let mine = conf_d.join("10-mine.toml");

    let (_watch, rx) = watch(cfg.clone(), 50);

    std::fs::write(
        &mine,
        "[daemon.providers.demo.sources.s]\nreturn = \"{{ result | poll('9s') }}\"\n",
    )
    .unwrap();
    touch_future(&mine);
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(3)),
        Ok(ChangeKind::Settings)
    );
}
