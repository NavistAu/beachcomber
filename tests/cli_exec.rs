//! `comb eval` and `comb get` evaluate user-initiated expressions with
//! `exec()` enabled — the CLI user already has a shell, so `exec()` is
//! available to both surfaces.
//!
//! The `eval` test uses the no-daemon harness `tests/cli_eval.rs`
//! establishes: a bogus `BEACHCOMBER_SOCKET` proves no daemon is contacted.
//! The `get` tests declare their template through `comb put <p>:template`
//! against a test daemon, the one way a template exists.

mod common;
use common::daemon::TestDaemon;

/// A `comb` at `/`, pointed at `d`'s socket and its temp `$HOME`/config dir.
fn comb(d: &TestDaemon) -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("comb").unwrap();
    cmd.env("BEACHCOMBER_SOCKET", &d.socket.path)
        .env("RUST_LOG", "error")
        .env("HOME", &d.home)
        .env("XDG_CONFIG_HOME", &d.xdg)
        .current_dir("/");
    cmd
}

/// `comb eval '{{ exec("sh","-c","echo hi") }}'` runs the command and
/// prints its output.
#[test]
fn eval_of_exec_call_prints_command_output() {
    let dir = tempfile::TempDir::new().unwrap();
    let bogus_sock = dir.path().join("nonexistent.sock");
    let mut cmd = assert_cmd::Command::cargo_bin("comb").unwrap();
    cmd.env("BEACHCOMBER_SOCKET", &bogus_sock)
        .env("RUST_LOG", "error")
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("cfg"))
        .args(["eval", r#"{{ exec("sh", "-c", "echo hi") }}"#]);
    cmd.assert().success().stdout(predicates::str::diff("hi"));
}

/// `comb get demo.shout` renders an exec()-backed template with `exec()`
/// enabled — `comb` is the user's own shell surface.
#[test]
fn get_of_template_with_exec_prints_command_output() {
    let d = TestDaemon::spawn();

    comb(&d)
        .args([
            "put",
            "demo:template.shout",
            r#""{{ exec('sh', '-c', 'echo ok') }}""#,
        ])
        .assert()
        .success();

    comb(&d)
        .args(["get", "demo.shout"])
        .assert()
        .success()
        .stdout(predicates::str::diff("ok"));
}

/// An exec()-backed template runs its command exactly once per `comb get`,
/// whatever else its cascade reaches for.
///
/// `demo.shout`'s expression is `exec(<script>) or cache.otherprov.otherfield`:
/// the script always prints nothing (falsy), so the cascade always falls
/// through to the daemon-backed term. The script's only side effect is
/// appending one line to `counter`; counting `counter`'s lines after one
/// `comb get` pins the number of real spawns.
#[test]
fn get_of_exec_or_cache_cascade_spawns_command_exactly_once() {
    let d = TestDaemon::spawn();

    let dir = tempfile::TempDir::new().unwrap();
    let counter = dir.path().join("counter.txt");
    let script = dir.path().join("counter.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\necho x >> \"{}\"\n", counter.display()),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    comb(&d)
        .args([
            "put",
            "demo:template.shout",
            &format!(
                "\"{{{{ exec(\\\"{}\\\") or cache.otherprov.otherfield }}}}\"",
                script.display()
            ),
        ])
        .assert()
        .success();

    // Seed the daemon-backed fallback the cascade falls through to.
    comb(&d)
        .args(["put", "otherprov", r#"{"otherfield":"daemon-value"}"#])
        .assert()
        .success();

    comb(&d)
        .args(["get", "demo.shout"])
        .assert()
        .success()
        .stdout(predicates::str::contains("daemon-value"));

    let logged = std::fs::read_to_string(&counter).unwrap_or_default();
    let spawn_count = logged.lines().count();
    assert_eq!(
        spawn_count, 1,
        "exec()-backed template must spawn exactly once per `comb get`; \
         counter file contents: {logged:?}"
    );
}
