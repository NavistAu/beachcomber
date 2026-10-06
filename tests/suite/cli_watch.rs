//! End-to-end tests for `comb watch`: its path handling (a cwd default, a
//! positional path, `--path`, and `--global`, like `get`/`refresh` — Plan 4
//! Task 1) and its resolved semantics (Plan 4 Task 3 — every frame is the
//! value `comb get` would print, re-resolved over every key the resolution
//! reads plus the provider's template tree).
//!
//! `comb watch` streams forever, so a streaming case here spawns it as a
//! child with piped stdout and reads its lines on a background thread
//! bounded by `recv_timeout` (no sleeps — see
//! `tests/suite/no_sleep_in_tests.rs`), then kills it. The two error cases
//! (conflicting path spellings, a deleted cwd) exit before any streaming
//! starts, so they use `assert_cmd` like `tests/suite/cli_refresh.rs` does
//! for the same cases.

use crate::common::daemon::{TestDaemon, isolate};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// The template tree these tests watch through: one leaf, rendered from a
/// literal tag so no environment is involved.
const TREE: &str = r#"{"widgets":{"foo":"{{ 'hi' }}"}}"#;

/// `TREE` with the leaf edited — what a `put` to the template makes the
/// watched value become.
const TREE_EDITED: &str = r#"{"widgets":{"foo":"{{ 'hi!' }}"}}"#;

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
/// stdout and stderr.
fn spawn_watch(d: &TestDaemon, cwd: &Path, args: &[&str]) -> WatchChild {
    let mut cmd = d.command(cwd);
    cmd.arg("watch")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    WatchChild(cmd.spawn().expect("spawn comb watch"))
}

/// A running `comb watch` whose stdout lines arrive on a channel fed by a
/// background thread, so a test can wait for one frame, provoke the next,
/// and wait again — all on bounded `recv_timeout`s, never a sleep.
struct WatchLines {
    child: WatchChild,
    rx: mpsc::Receiver<String>,
}

/// Spawn `comb watch <args>` at `cwd` and start draining its stdout.
fn watch_lines(d: &TestDaemon, cwd: &Path, args: &[&str]) -> WatchLines {
    let mut child = spawn_watch(d, cwd, args);
    let stdout = child.0.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { return };
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    WatchLines { child, rx }
}

impl WatchLines {
    /// The next frame's line, waiting up to 5s (the suite's norm for
    /// spawn+connect+first-frame waits). A timeout panics naming what the
    /// child wrote to stderr rather than failing as a bare `None`.
    fn next_line(&mut self) -> String {
        match self.rx.recv_timeout(Duration::from_secs(5)) {
            Ok(line) => line,
            Err(_) => panic!(
                "comb watch produced no further output within 5s; stderr:\n{}",
                self.stderr()
            ),
        }
    }

    fn stderr(&mut self) -> String {
        let _ = self.child.0.kill();
        let mut stderr = String::new();
        if let Some(mut s) = self.child.0.stderr.take() {
            let _ = s.read_to_string(&mut stderr);
        }
        let _ = self.child.0.wait();
        stderr
    }
}

/// The first frame of `comb watch <args>` at `cwd`, then the child is killed.
fn first_line(d: &TestDaemon, cwd: &Path, args: &[&str]) -> String {
    watch_lines(d, cwd, args).next_line()
}

#[test]
fn watch_defaults_to_the_callers_cwd() {
    let d = TestDaemon::spawn();
    let project = d.home.join("ws/project");
    std::fs::create_dir_all(&project).unwrap();

    d.comb(&project)
        .args(["put", "my", r#"{"a":1}"#])
        .assert()
        .success();

    assert_eq!(first_line(&d, &project, &["my.a"]), "1");
}

/// A put scoped to `project` doesn't merge into `other`'s chain, so a watch
/// at `other` starts on a miss — and a resolved watch emits nothing for a
/// miss, where the pre-Task-3 watch printed `render_data(None, _)`, a blank
/// line. The first line to arrive is therefore `other`'s own later value,
/// which pins both halves at once: no blank frame came first, and the `1` at
/// the sibling was never visible.
#[test]
fn watch_from_a_sibling_directory_does_not_see_that_directorys_value() {
    let d = TestDaemon::spawn();
    let project = d.home.join("ws/project");
    let other = d.home.join("ws/other");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&other).unwrap();

    d.comb(&project)
        .args(["put", "my", r#"{"a":1}"#])
        .assert()
        .success();

    let mut w = watch_lines(&d, &other, &["my.a"]);
    d.comb(&other)
        .args(["put", "my", r#"{"a":2}"#])
        .assert()
        .success();
    assert_eq!(w.next_line(), "2");
}

#[test]
fn watch_global_reads_the_home_entry_from_anywhere() {
    let d = TestDaemon::spawn();
    let elsewhere = d.home.join("ws/other");
    std::fs::create_dir_all(&elsewhere).unwrap();

    d.comb(&d.home)
        .args(["put", "--global", "g", r#"{"v":2}"#])
        .assert()
        .success();

    assert_eq!(first_line(&d, &elsewhere, &["--global", "g.v"]), "2");
}

#[test]
fn watch_takes_a_relative_positional_path() {
    let d = TestDaemon::spawn();
    let ws = d.home.join("ws");
    let project = ws.join("project");
    std::fs::create_dir_all(&project).unwrap();

    d.comb(&project)
        .args(["put", "my", r#"{"a":1}"#])
        .assert()
        .success();

    assert_eq!(first_line(&d, &ws, &["my.a", "./project"]), "1");
}

#[test]
fn watch_takes_the_path_as_a_flag() {
    let d = TestDaemon::spawn();
    let project = d.home.join("ws/project");
    std::fs::create_dir_all(&project).unwrap();

    d.comb(&project)
        .args(["put", "my", r#"{"a":1}"#])
        .assert()
        .success();

    let project_str = project.to_str().unwrap();
    assert_eq!(
        first_line(&d, &d.home, &["my.a", "--path", project_str]),
        "1"
    );
}

/// `--path <dir>` is the positional's other spelling, and the three ways of
/// naming the entry exclude each other — mirrors
/// `tests/suite/cli_refresh.rs::refresh_takes_the_path_as_a_flag_and_rejects_two_spellings_at_once`.
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
        d.comb(&d.home)
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
    let mut cmd = std::process::Command::new("sh");
    isolate(&mut cmd, &d.home, &d.xdg, &d.socket.path)
        .env("COMB", env!("CARGO_BIN_EXE_comb"))
        .args([
            "-c",
            r#"cd "$1" && rmdir "$1" && exec "$COMB" watch my.a"#,
            "sh",
        ])
        .arg(&gone);
    assert_cmd::Command::from_std(cmd)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "cannot determine the current directory",
        ));
}

/// The same deleted-cwd case, but with an explicit relative positional (`.`)
/// rather than relying on the default — covers the relative branch of
/// `effective_cli_path`, where `watch_from_a_deleted_cwd_with_no_path_is_an_error`
/// above covers the `None` branch.
#[test]
fn watch_from_a_deleted_cwd_with_a_relative_path_is_an_error() {
    let d = TestDaemon::spawn();
    let gone = d.home.join("gone");
    std::fs::create_dir_all(&gone).unwrap();

    // `sh` enters the directory, removes it from under itself, and execs
    // `comb` there; `Command::current_dir` on a missing directory would fail
    // at spawn instead.
    let mut cmd = std::process::Command::new("sh");
    isolate(&mut cmd, &d.home, &d.xdg, &d.socket.path)
        .env("COMB", env!("CARGO_BIN_EXE_comb"))
        .args([
            "-c",
            r#"cd "$1" && rmdir "$1" && exec "$COMB" watch my.a ."#,
            "sh",
        ])
        .arg(&gone);
    assert_cmd::Command::from_std(cmd)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "cannot determine the current directory",
        ));
}

// ── Plan 4 Task 3: every frame is the resolved value ────────────────────────

/// The watched value is what `comb get` would print — a template leaf
/// rendered, not the raw tag — and editing the template the value is
/// rendered from fires the watch again with the new rendering. The template
/// tree is not a key the addressed watch would ever cover on its own: the
/// fan-in subscribes to it because the resolution read it.
#[test]
fn watch_emits_the_resolved_value_and_refires_when_its_template_changes() {
    let d = TestDaemon::spawn();
    let project = d.home.join("ws/project");
    std::fs::create_dir_all(&project).unwrap();

    d.comb(&project)
        .args(["put", "scanline:template", TREE])
        .assert()
        .success();

    let mut w = watch_lines(&d, &project, &["scanline.widgets.foo"]);
    assert_eq!(w.next_line(), "hi");

    d.comb(&project)
        .args(["put", "scanline:template", TREE_EDITED])
        .assert()
        .success();
    assert_eq!(w.next_line(), "hi!");
}

/// `env.*` is constant for the life of a process, so there is nothing to
/// watch and the library says so before any connection is made. The CLI
/// prints the library's words and exits 2.
#[test]
fn watch_refuses_an_env_key() {
    let d = TestDaemon::spawn();

    d.comb(&d.home)
        .args(["watch", "env.HOME"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("env keys cannot be watched"));
}

/// A facet key is already inside a facet: there is no template half to
/// render, so its frames are the daemon's own — `scanline:template` streams
/// the raw tree, tags and all, exactly as `comb get scanline:template -f
/// json` prints it.
#[test]
fn watch_on_a_template_facet_streams_the_raw_tree() {
    let d = TestDaemon::spawn();
    let project = d.home.join("ws/project");
    std::fs::create_dir_all(&project).unwrap();

    d.comb(&project)
        .args(["put", "scanline:template", TREE])
        .assert()
        .success();

    let line = first_line(&d, &project, &["scanline:template", "-f", "json"]);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap(),
        serde_json::json!({"widgets": {"foo": "{{ 'hi' }}"}})
    );
}

/// `-f json` frames are the bare value, one JSON document per line — not the
/// `{"ok":…,"data":…,"age_ms":…}` response envelope the daemon puts on the
/// wire, which is what `watch` used to print.
#[test]
fn watch_json_frames_are_bare_values() {
    let d = TestDaemon::spawn();
    let project = d.home.join("ws/project");
    std::fs::create_dir_all(&project).unwrap();

    d.comb(&project)
        .args(["put", "scanline:template", TREE])
        .assert()
        .success();

    let mut w = watch_lines(&d, &project, &["scanline.widgets.foo", "-f", "json"]);
    assert_eq!(w.next_line(), r#""hi""#);

    d.comb(&project)
        .args(["put", "scanline:template", TREE_EDITED])
        .assert()
        .success();
    assert_eq!(w.next_line(), r#""hi!""#);
}
