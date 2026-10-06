//! End-to-end tests for `comb eval`: it is a client of
//! `libbeachcomber::resolve`, exactly as `comb get` is, so every ref inside
//! its template — `env.*`, a plain `provider.field`, a `cache.*` ref, a
//! nested template field — resolves through the one resolver. `comb` always
//! requires the daemon (design §2, outcome 22): there is no source that
//! skips starting one, `env.*`-only included.
//!
//! Reference discovery is `libbeachcomber::eval::discover_refs`, covered in
//! `libbeachcomber/tests/suite/eval_classify.rs` and `tests/template.rs`. The
//! bulk of `eval`'s render-contract tests (literal text, typed single-tag
//! results, the `default` filter on a miss, …) live in `tests/suite/cli_golden.rs`.

use crate::common::daemon::{TestDaemon, isolate};
use crate::common::socket::IsolatedSocket;
use std::path::{Path, PathBuf};

// ── outcome 22: `comb eval` always starts the daemon ─────────────────────────

/// Stops the daemon `comb eval` started on its own, so a test never leaves
/// one behind: `ensure_daemon` forks without `--exit-with-parent`. `kill`
/// gets the same temp `$HOME`/`$XDG_CONFIG_HOME` as the `eval` that started
/// the daemon, like every other `comb` spawn in this file — the developer's
/// real config dir must never be touched by a test.
struct SpawnedDaemon {
    socket: IsolatedSocket,
    home: PathBuf,
    xdg: PathBuf,
}

impl Drop for SpawnedDaemon {
    fn drop(&mut self) {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_comb"));
        let _ = isolate(&mut cmd, &self.home, &self.xdg)
            .env("RUST_LOG", "error")
            .args(["kill", "--socket"])
            .arg(&self.socket.path)
            .output();
    }
}

/// A source needing only `env.*` still starts the daemon: `comb` always
/// requires one (design §2), no source is exempt. Before this task, an
/// empty daemon-ref closure skipped `ensure_daemon` entirely.
#[test]
fn eval_starts_the_daemon() {
    let socket = IsolatedSocket::new();
    let home = socket.dir.path().join("home");
    let xdg = socket.dir.path().join("xdg");
    std::fs::create_dir_all(&home).unwrap();
    let guard = SpawnedDaemon {
        socket,
        home: home.clone(),
        xdg: xdg.clone(),
    };
    assert!(!guard.socket.path.exists(), "no daemon before eval runs");

    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_comb"));
    isolate(&mut cmd, &home, &xdg)
        .env("BEACHCOMBER_SOCKET", &guard.socket.path)
        .env("RUST_LOG", "error")
        .env("FOO", "hi")
        .args(["eval", "{{ env.FOO }}"]);
    assert_cmd::Command::from_std(cmd)
        .assert()
        .success()
        .stdout(predicates::str::diff("hi"));

    assert!(
        guard.socket.path.exists(),
        "comb eval must start the daemon (outcome 22)"
    );
}

// ── refs resolve through the same daemon-served template store `get` uses ────

/// `aws.profile` is a managed default (`defaults/aws.toml`, seeded by every
/// daemon on start) — a template served under `aws:template`, not a
/// hand-declared field. `eval` fetches it from the daemon and renders it with
/// the calling shell's environment, same as `comb get aws.profile` would.
#[test]
fn eval_resolves_daemon_served_template_from_env() {
    let d = TestDaemon::spawn();

    d.comb(Path::new("/"))
        .env("AWS_PROFILE", "prod-account")
        .args(["eval", "{{ aws.profile }}"])
        .assert()
        .success()
        .stdout(predicates::str::contains("prod-account"));
}

/// `comb eval '{{ scanline.widgets.foo }}'` resolves a field nested two
/// levels inside a stored template exactly as `comb get scanline.widgets.foo`
/// does (design §3.1's worked example) — the two must agree byte-for-byte.
#[test]
fn eval_of_nested_template_field_matches_get() {
    let d = TestDaemon::spawn();

    d.comb(Path::new("/"))
        .args([
            "put",
            "scanline:template",
            r#"{"widgets":{"foo":"{{ env.SCANLINE_FOO }}"}}"#,
        ])
        .assert()
        .success();

    let get_out = d
        .comb(Path::new("/"))
        .env("SCANLINE_FOO", "hi")
        .args(["get", "scanline.widgets.foo"])
        .output()
        .unwrap();
    let get_out = String::from_utf8(get_out.stdout).unwrap();
    assert_eq!(get_out, "hi");

    d.comb(Path::new("/"))
        .env("SCANLINE_FOO", "hi")
        .args(["eval", "{{ scanline.widgets.foo }}"])
        .assert()
        .success()
        .stdout(predicates::str::diff(get_out));
}

/// A ref the daemon rejects — `nope` names no registered provider, no `put`,
/// no template — is a missing ref inside a template, not an error: it binds
/// nothing, so `or` falls through to its other side (canon: a missing ref is
/// falsy at any depth).
#[test]
fn eval_rejected_ref_is_falsy() {
    let d = TestDaemon::spawn();

    d.comb(Path::new("/"))
        .args(["eval", r#"{{ cache.nope.x or "fb" }}"#])
        .assert()
        .success()
        .stdout(predicates::str::diff("fb"));
}

/// A source that does not compile is the caller's to fix: `1 +` is not a
/// valid minijinja expression, so `eval` exits 2 naming the failure as a
/// compile error rather than trying to render anything.
#[test]
fn eval_compile_error_exits_2_naming_it() {
    let d = TestDaemon::spawn();

    d.comb(Path::new("/"))
        .args(["eval", "{{ 1 + }}"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("compile error"));
}

/// `comb eval` runs with `allow_exec = true` — the CLI user already has a
/// shell, so `exec()` is available to it, same as `comb get`.
#[test]
fn eval_exec_call_is_enabled_for_the_cli() {
    let d = TestDaemon::spawn();

    d.comb(Path::new("/"))
        .args(["eval", r#"{{ exec("printf", "x") }}"#])
        .assert()
        .success()
        .stdout(predicates::str::diff("x"));
}

/// `comb eval`'s output goes through `render::render_data`, which is a bare
/// passthrough for a string value — no ANSI/control-character stripping.
/// The `fg` filter's ESC bytes must survive to stdout unmangled so a shell
/// prompt piping `comb eval` output gets real colour, not a sanitized
/// version of it.
#[test]
fn eval_fg_filter_escapes_pass_through_unstripped() {
    let d = TestDaemon::spawn();

    d.comb(Path::new("/"))
        .env("FOO", "hi")
        .args(["eval", "{{ env.FOO | fg('#00ff00') }}"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\x1b[38;2;0;255;0mhi\x1b[0m"));
}
