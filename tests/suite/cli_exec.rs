//! `comb eval` and `comb get` evaluate user-initiated expressions with
//! `exec()` enabled — the CLI user already has a shell, so `exec()` is
//! available to both surfaces.
//!
//! `comb` always requires the daemon (design §2, outcome 22), so every test
//! here runs against `TestDaemon`, which is killed in `Drop` — no test in
//! this repo may leave a daemon running. The `get` tests declare their
//! template through `comb put <p>:template` against it, the one way a
//! template exists.

use crate::common::daemon::TestDaemon;
use std::path::Path;

/// `comb eval '{{ exec("echo hi") }}'` runs the command and
/// prints its output.
#[test]
fn eval_of_exec_call_prints_command_output() {
    let d = TestDaemon::spawn();

    d.comb(Path::new("/"))
        .args(["eval", r#"{{ exec("echo hi") }}"#])
        .assert()
        .success()
        .stdout(predicates::str::diff("hi"));
}

/// `comb get demo.shout` renders an exec()-backed template with `exec()`
/// enabled — `comb` is the user's own shell surface.
#[test]
fn get_of_template_with_exec_prints_command_output() {
    let d = TestDaemon::spawn();

    d.comb(Path::new("/"))
        .args(["put", "demo:template.shout", r#""{{ exec('echo ok') }}""#])
        .assert()
        .success();

    d.comb(Path::new("/"))
        .args(["get", "demo.shout"])
        .assert()
        .success()
        .stdout(predicates::str::diff("ok"));
}

/// An exec()-backed template runs its command exactly once per `comb get`,
/// whatever else its cascade reaches for.
///
/// `demo.shout`'s expression is `exec(<script>) or comb.otherprov.otherfield`:
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

    d.comb(Path::new("/"))
        .args([
            "put",
            "demo:template.shout",
            &format!(
                "\"{{{{ exec(\\\"{}\\\") or comb.otherprov.otherfield }}}}\"",
                script.display()
            ),
        ])
        .assert()
        .success();

    // Seed the daemon-backed fallback the cascade falls through to.
    d.comb(Path::new("/"))
        .args(["put", "otherprov", r#"{"otherfield":"daemon-value"}"#])
        .assert()
        .success();

    d.comb(Path::new("/"))
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

const DISABLED: &str = "exec() is disabled in this context";

/// `--no-exec` renders a `get` with `exec()` off: the template fails, naming
/// why, and nothing is spawned.
#[test]
fn no_exec_get_of_an_exec_template_fails() {
    let d = TestDaemon::spawn();
    d.comb(Path::new("/"))
        .args(["put", "demo:template.shout", r#""{{ exec('echo ok') }}""#])
        .assert()
        .success();

    d.comb(Path::new("/"))
        .args(["--no-exec", "get", "demo.shout"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(DISABLED));
}

/// `--no-exec` on `eval`.
#[test]
fn no_exec_eval_fails() {
    let d = TestDaemon::spawn();
    d.comb(Path::new("/"))
        .args(["--no-exec", "eval", r#"{{ exec("echo hi") }}"#])
        .assert()
        .failure()
        .stderr(predicates::str::contains(DISABLED));
}

/// `--no-exec` on `watch`: the first frame's render fails, so the command
/// reports it and exits rather than streaming.
#[test]
fn no_exec_watch_fails() {
    let d = TestDaemon::spawn();
    d.comb(Path::new("/"))
        .args(["put", "demo:template.shout", r#""{{ exec('echo ok') }}""#])
        .assert()
        .success();

    d.comb(Path::new("/"))
        .args(["--no-exec", "watch", "demo.shout"])
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .failure()
        .stderr(predicates::str::contains(DISABLED));
}

/// `--no-exec` on `status`: a template row that needs `exec()` renders as
/// failed, with the reason in its cell.
#[test]
fn no_exec_status_row_renders_as_failed() {
    let d = TestDaemon::spawn();
    d.comb(Path::new("/"))
        .args(["put", "demo:template.shout", r#""{{ exec('echo ok') }}""#])
        .assert()
        .success();

    let out = |extra: &[&str]| {
        let o = d
            .comb(Path::new("/"))
            .args(extra)
            .args(["status", "-f", "json", "--filter", "provider=demo"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    assert!(out(&[]).contains("ok"), "without the flag the row renders");
    assert!(
        out(&["--no-exec"]).contains(DISABLED),
        "with it the row carries the failure"
    );
}

/// A string command runs under `sh -c` with the CLI process's own
/// environment: `$FOO` and `env.FOO` read the same value.
#[test]
fn comb_eval_exec_sees_the_cli_process_env() {
    let d = TestDaemon::spawn();

    d.comb(Path::new("/"))
        .env("FOO", "x")
        .args(["eval", r#"{{ exec('echo $FOO') ~ env.FOO }}"#])
        .assert()
        .success()
        .stdout(predicates::str::diff("xx"));
}
