//! `comb eval` and `comb get` evaluate user-initiated expressions with
//! `.with_exec()` enabled — the CLI user already has a shell, so `exec()`
//! is available to both surfaces.
//!
//! The first two tests use the no-daemon harness `tests/cli_eval.rs` and
//! `tests/cli_get_rerouting.rs` establish: a bogus `BEACHCOMBER_SOCKET`
//! proves no daemon is contacted. `exec()` itself makes no daemon-backed
//! reference, so neither test needs a running daemon.

mod common;
use common::daemon::TestDaemon;

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

/// `comb get demo.shout` resolves a config-declared exec()-backed virtual
/// field client-side (`evaluate_client_side` in `src/cli/commands/get.rs`)
/// — no daemon required, since the field's expression makes no
/// daemon-backed reference either.
#[test]
fn get_of_config_virtual_field_with_exec_prints_command_output() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = dir.path().join("cfg");
    std::fs::create_dir_all(cfg.join("beachcomber")).unwrap();
    std::fs::write(
        cfg.join("beachcomber").join("config.toml"),
        r#"
[providers.demo]
virtual."shout" = '{{ exec("sh", "-c", "echo ok") }}'
"#,
    )
    .unwrap();

    let bogus_sock = dir.path().join("nonexistent.sock");
    let mut cmd = assert_cmd::Command::cargo_bin("comb").unwrap();
    cmd.env("BEACHCOMBER_SOCKET", &bogus_sock)
        .env("RUST_LOG", "error")
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", &cfg)
        .args(["get", "demo.shout"]);
    cmd.assert().success().stdout(predicates::str::diff("ok"));
}

/// `run_get`'s env-first lazy strategy evaluates a virtual field with daemon
/// refs twice — once with empty `daemon_data` to see whether an env term
/// wins outright, and again with the real fetched data if it doesn't. An
/// exec()-backed field reachable from both evaluations must still run its
/// command exactly once, not twice: `evaluate_client_side`'s probe call
/// passes `allow_exec: false`, so the probe's `exec()` call errors with the
/// disabled message (falsy-equivalent for "did an env term win") instead of
/// actually spawning.
///
/// `demo.shout`'s expression is `exec(<script>) or cache.otherprov.otherfield`:
/// the script always prints nothing (falsy), so the cascade always falls
/// through to the daemon-backed term regardless of which pass is running —
/// forcing both passes to actually reach the `exec()` call whenever `exec()`
/// is enabled for them. The script's only side effect is appending one line
/// to `counter`; counting `counter`'s lines after one `comb get` pins the
/// number of real spawns.
#[test]
fn get_of_exec_or_cache_cascade_spawns_command_exactly_once() {
    let daemon = TestDaemon::spawn();

    let dir = tempfile::TempDir::new().unwrap();
    let cfg = dir.path().join("cfg");
    std::fs::create_dir_all(cfg.join("beachcomber")).unwrap();

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

    std::fs::write(
        cfg.join("beachcomber").join("config.toml"),
        format!(
            "\n[providers.demo]\nvirtual.\"shout\" = '{{{{ exec(\"{}\") or cache.otherprov.otherfield }}}}'\n",
            script.display()
        ),
    )
    .unwrap();

    // Seed the daemon-backed fallback the cascade falls through to.
    let mut put_cmd = assert_cmd::Command::cargo_bin("comb").unwrap();
    put_cmd
        .env("BEACHCOMBER_SOCKET", &daemon.socket.path)
        .env("RUST_LOG", "error")
        .current_dir("/")
        .args([
            "put",
            "otherprov",
            r#"{"otherfield":"daemon-value"}"#,
            "--path",
            "/",
        ]);
    put_cmd.assert().success();

    let mut get_cmd = assert_cmd::Command::cargo_bin("comb").unwrap();
    get_cmd
        .env("BEACHCOMBER_SOCKET", &daemon.socket.path)
        .env("RUST_LOG", "error")
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", &cfg)
        .current_dir("/")
        .args(["get", "demo.shout"]);
    get_cmd
        .assert()
        .success()
        .stdout(predicates::str::contains("daemon-value"));

    let logged = std::fs::read_to_string(&counter).unwrap_or_default();
    let spawn_count = logged.lines().count();
    assert_eq!(
        spawn_count, 1,
        "exec()-backed field must spawn exactly once per `comb get`, not once per \
         env-first-lazy pass; counter file contents: {logged:?}"
    );
}
