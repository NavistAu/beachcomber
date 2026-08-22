//! Integration tests for the daemon's config-file self-watch
//! (`src/daemon/config_watch.rs`), which mirrors the binary self-watch
//! (`tests/binary_self_watch.rs`) but only restarts when the changed file
//! parses cleanly.
//!
//! These use the polling backend (`fs_events: false`) so they run reliably
//! under sandboxed hosts that can't use FSEvents / inotify, same convention
//! as the watcher tests in `tests/`.

use beachcomber::daemon::config_watch::spawn_config_self_watch_with;
use std::time::Duration;

/// Bump `path`'s mtime strictly into the future, immune to clock granularity
/// (mirrors the trick in `tests/binary_self_watch.rs`).
fn touch_future(path: &std::path::Path) {
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(std::time::SystemTime::now() + Duration::from_secs(2))
        .unwrap();
}

#[test]
fn poll_restarts_on_valid_config_change_when_fs_events_are_dead() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml");
    std::fs::write(&cfg, "[daemon]\n").unwrap();

    let (tx, rx) = std::sync::mpsc::channel();
    spawn_config_self_watch_with(
        cfg.clone(),
        Duration::from_millis(100),
        false, // fs events dead — the poll is the only mechanism
        move || {
            let _ = tx.send(());
        },
    );

    std::thread::sleep(Duration::from_millis(50));
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

    let (tx, rx) = std::sync::mpsc::channel();
    spawn_config_self_watch_with(cfg.clone(), Duration::from_millis(100), false, move || {
        let _ = tx.send(());
    });

    std::thread::sleep(Duration::from_millis(50));
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

    let (tx, rx) = std::sync::mpsc::channel();
    spawn_config_self_watch_with(cfg.clone(), Duration::from_millis(100), false, move || {
        let _ = tx.send(());
    });

    std::thread::sleep(Duration::from_millis(50));
    std::fs::write(&cfg, "[daemon]\n").unwrap();

    rx.recv_timeout(Duration::from_secs(3))
        .expect("config file created after startup should be picked up");
}

#[test]
fn config_file_that_never_appears_never_restarts() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("never-written.toml");

    let (tx, rx) = std::sync::mpsc::channel();
    spawn_config_self_watch_with(cfg, Duration::from_millis(100), false, move || {
        let _ = tx.send(());
    });

    let result = rx.recv_timeout(Duration::from_millis(500));
    assert!(
        result.is_err(),
        "no config file ever appeared; daemon must not restart"
    );
}
