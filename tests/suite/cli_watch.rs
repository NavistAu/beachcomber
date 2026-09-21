//! End-to-end tests for `comb watch`'s path handling: a cwd default, a
//! positional path, `--path`, and `--global`, like `get`/`refresh` — added
//! in Plan 4 Task 1, which gave `watch` the shared `effective_cli_path`
//! resolver it previously lacked.
//!
//! `comb watch` streams forever, so a hit/miss case here spawns it as a
//! child with piped stdout, reads only the first line on a background
//! thread bounded by `recv_timeout` (no sleeps — see
//! `tests/no_sleep_in_tests.rs`), then kills it. The daemon's watch
//! protocol itself is covered by
//! `tests/cli_golden.rs::golden_watch_emits_value_then_exits`; these tests
//! prove the path resolution alone. The two error cases (conflicting path
//! spellings, a deleted cwd) exit before any streaming starts, so they use
//! `assert_cmd` like `tests/cli_refresh.rs` does for the same cases.

use crate::common::daemon::TestDaemon;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// A `comb` at `cwd`, pointed at `d`'s socket and its temp `$HOME`/config dir.
fn comb(d: &TestDaemon, cwd: &Path) -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("comb").unwrap();
    cmd.env("BEACHCOMBER_SOCKET", &d.socket.path)
        .env("RUST_LOG", "error")
        .env("HOME", &d.home)
        .env("XDG_CONFIG_HOME", &d.xdg)
        .env("XDG_STATE_HOME", d.home.join(".local/state"))
        .current_dir(cwd);
    cmd
}

/// A running `comb watch` child, killed and reaped on drop so an early
/// `assert!` failure in a test body never leaks a daemon-attached process.
struct WatchChild(Child);

impl Drop for WatchChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Spawn `comb watch <args>` at `cwd`, isolated like `comb` above, with piped
/// stdout.
fn spawn_watch(d: &TestDaemon, cwd: &Path, args: &[&str]) -> WatchChild {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_comb"));
    cmd.env("BEACHCOMBER_SOCKET", &d.socket.path)
        .env("RUST_LOG", "error")
        .env("HOME", &d.home)
        .env("XDG_CONFIG_HOME", &d.xdg)
        .env("XDG_STATE_HOME", d.home.join(".local/state"))
        .current_dir(cwd)
        .arg("watch")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    WatchChild(cmd.spawn().expect("spawn comb watch"))
}

/// Read one line from `child`'s stdout on a background thread, waiting up to
/// 2s. `None` if nothing arrives in time — distinct from `Some("")`, a miss
/// frame's blank line.
fn first_line(child: &mut WatchChild) -> Option<String> {
    let stdout = child.0.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        if BufReader::new(stdout).read_line(&mut line).unwrap_or(0) > 0 {
            let _ = tx.send(line.trim_end_matches('\n').to_string());
        }
    });
    rx.recv_timeout(Duration::from_secs(2)).ok()
}

#[test]
fn watch_defaults_to_the_callers_cwd() {
    let d = TestDaemon::spawn();
    let project = d.home.join("ws/project");
    std::fs::create_dir_all(&project).unwrap();

    comb(&d, &project)
        .args(["put", "my", r#"{"a":1}"#])
        .assert()
        .success();

    let mut child = spawn_watch(&d, &project, &["my.a"]);
    assert_eq!(first_line(&mut child).as_deref(), Some("1"));
}

#[test]
fn watch_from_a_sibling_directory_is_a_miss() {
    let d = TestDaemon::spawn();
    let project = d.home.join("ws/project");
    let other = d.home.join("ws/other");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&other).unwrap();

    comb(&d, &project)
        .args(["put", "my", r#"{"a":1}"#])
        .assert()
        .success();

    // A put scoped to `project` doesn't merge into `other`'s chain, so the
    // initial watch frame is a miss — `run_watch` prints `render_data(None, _)`,
    // an empty line, not nothing.
    let mut child = spawn_watch(&d, &other, &["my.a"]);
    assert_eq!(first_line(&mut child).as_deref(), Some(""));
}

#[test]
fn watch_global_reads_the_home_entry_from_anywhere() {
    let d = TestDaemon::spawn();
    let elsewhere = d.home.join("ws/other");
    std::fs::create_dir_all(&elsewhere).unwrap();

    comb(&d, &d.home)
        .args(["put", "--global", "g", r#"{"v":2}"#])
        .assert()
        .success();

    let mut child = spawn_watch(&d, &elsewhere, &["--global", "g.v"]);
    assert_eq!(first_line(&mut child).as_deref(), Some("2"));
}

#[test]
fn watch_takes_a_relative_positional_path() {
    let d = TestDaemon::spawn();
    let ws = d.home.join("ws");
    let project = ws.join("project");
    std::fs::create_dir_all(&project).unwrap();

    comb(&d, &project)
        .args(["put", "my", r#"{"a":1}"#])
        .assert()
        .success();

    let mut child = spawn_watch(&d, &ws, &["my.a", "./project"]);
    assert_eq!(first_line(&mut child).as_deref(), Some("1"));
}

#[test]
fn watch_takes_the_path_as_a_flag() {
    let d = TestDaemon::spawn();
    let project = d.home.join("ws/project");
    std::fs::create_dir_all(&project).unwrap();

    comb(&d, &project)
        .args(["put", "my", r#"{"a":1}"#])
        .assert()
        .success();

    let project_str = project.to_str().unwrap();
    let mut child = spawn_watch(&d, &d.home, &["my.a", "--path", project_str]);
    assert_eq!(first_line(&mut child).as_deref(), Some("1"));
}

/// `--path <dir>` is the positional's other spelling, and the three ways of
/// naming the entry exclude each other — mirrors
/// `tests/cli_refresh.rs::refresh_takes_the_path_as_a_flag_and_rejects_two_spellings_at_once`.
#[test]
fn watch_rejects_two_spellings_of_a_path_at_once() {
    let d = TestDaemon::spawn();
    let home = d.home.to_str().unwrap();

    let rejected: [&[&str]; 3] = [
        &["watch", "my.a", home, "--path", home],
        &["watch", "my.a", "--global", "--path", home],
        &["watch", "my.a", "--global", home],
    ];
    for args in rejected {
        comb(&d, &d.home)
            .args(args)
            .assert()
            .code(2)
            .stderr(predicates::str::contains("cannot be used with"));
    }
}

/// The cwd is read only when it is needed, and when it is needed and gone the
/// answer is an error — never a silent watch of the global entry.
#[test]
fn watch_from_a_deleted_cwd_with_no_path_is_an_error() {
    let d = TestDaemon::spawn();
    let gone = d.home.join("gone");
    std::fs::create_dir_all(&gone).unwrap();

    // `sh` enters the directory, removes it from under itself, and execs
    // `comb` there; `Command::current_dir` on a missing directory would fail
    // at spawn instead.
    let mut cmd = assert_cmd::Command::new("sh");
    cmd.env("BEACHCOMBER_SOCKET", &d.socket.path)
        .env("RUST_LOG", "error")
        .env("HOME", &d.home)
        .env("XDG_CONFIG_HOME", &d.xdg)
        .env("XDG_STATE_HOME", d.home.join(".local/state"))
        .env("COMB", env!("CARGO_BIN_EXE_comb"))
        .args([
            "-c",
            r#"cd "$1" && rmdir "$1" && exec "$COMB" watch my.a"#,
            "sh",
        ])
        .arg(&gone);
    cmd.assert().code(2).stderr(predicates::str::contains(
        "cannot determine the current directory",
    ));
}
