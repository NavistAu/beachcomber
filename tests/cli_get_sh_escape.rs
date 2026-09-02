//! Canon invariant 8: `--format text`/`sh` stringify a resolved value, and `sh`
//! shell-escapes it. Object output (a whole-provider or object-valued query)
//! must escape each value too, not only scalars.
//!
//! Two rendering paths reach `-f sh` and both are covered here:
//!
//! - **client-side** — `conda` is the cleanest probe: a pure-virtual namespace
//!   (no daemon provider behind it), so `comb get conda` renders through the
//!   CLI's own object formatter with no socket in play.
//! - **daemon-backed** — a `put`-created provider has no virtual fields, so
//!   `comb get -f sh <prov>` short-circuits to `Client::get_formatted_with_flags`
//!   and renders through `libbeachcomber::render::render_data`.

mod common;

use assert_cmd::Command;
use common::daemon::TestDaemon;
use tempfile::TempDir;

/// Run `comb get -f <format> conda` with `CONDA_DEFAULT_ENV` set to `value`,
/// against a socket that does not exist (no daemon is contacted for `conda`).
fn conda_output(format: &str, value: &str) -> String {
    let dir = TempDir::new().unwrap();
    let bogus_sock = dir.path().join("nonexistent.sock");
    let mut cmd = Command::cargo_bin("comb").unwrap();
    cmd.env("BEACHCOMBER_SOCKET", &bogus_sock)
        .env("RUST_LOG", "error")
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("cfg"))
        .env("CONDA_DEFAULT_ENV", value)
        .args(["get", "-f", format, "conda"]);
    let assert = cmd.assert().success();
    String::from_utf8(assert.get_output().stdout.clone()).unwrap()
}

/// A value carrying a space, a single quote and a `$` must come back
/// POSIX-single-quoted so the line is safe to `eval`.
#[test]
fn sh_format_shell_escapes_object_values() {
    let out = conda_output("sh", "it's a $HOME env");
    assert_eq!(
        out, "env='it'\\''s a $HOME env'\n",
        "-f sh must shell-escape each object value (canon invariant 8)"
    );
}

/// The escaped line survives a round trip through `eval` in a real shell.
#[test]
fn sh_format_object_line_is_eval_safe() {
    let out = conda_output("sh", "it's a $HOME env");
    let line = out.trim_end_matches('\n');
    // Exactly how a shell consumes this: `eval "$(comb get -f sh conda)"`.
    // HOME is unset so a `$HOME` that wrongly expanded would show up as a gap.
    let shell = std::process::Command::new("sh")
        .args(["-c", r#"eval "$COMB_SH_LINE"; printf %s "$env""#])
        .env("COMB_SH_LINE", line)
        .env_remove("HOME")
        .output()
        .expect("sh runs");
    assert_eq!(
        String::from_utf8(shell.stdout).unwrap(),
        "it's a $HOME env",
        "the escaped value must survive `eval` byte-for-byte"
    );
}

/// Text output is unchanged: values stay raw.
#[test]
fn text_format_leaves_object_values_raw() {
    let out = conda_output("text", "it's a $HOME env");
    assert_eq!(
        out, "env=it's a $HOME env\n",
        "-f text must not shell-escape"
    );
}

// ── daemon-backed path (`render::render_data`) ───────────────────────────────

/// The nasty value every daemon-backed case below round-trips: a space, a
/// single quote and a `$` — each of which breaks a bare `eval`.
const HOSTILE: &str = "it's a $HOME env";

/// Seed `shq` (a `put`-created provider, so no virtual fields and no
/// client-side resolution) and run `comb get -f <format> <key>` against it.
fn daemon_output(d: &TestDaemon, format: &str, key: &str) -> String {
    let mut cmd = Command::cargo_bin("comb").unwrap();
    cmd.env("BEACHCOMBER_SOCKET", &d.socket.path)
        .env("RUST_LOG", "error")
        .current_dir("/")
        .args(["get", "-f", format, key]);
    let assert = cmd.assert().success();
    String::from_utf8(assert.get_output().stdout.clone()).unwrap()
}

/// `put` the fixture provider under `--path /` (matching the fixed CWD the
/// helper above queries from).
fn seed(d: &TestDaemon) {
    let payload = serde_json::json!({ "one": HOSTILE, "two": 42 }).to_string();
    Command::cargo_bin("comb")
        .unwrap()
        .env("BEACHCOMBER_SOCKET", &d.socket.path)
        .env("RUST_LOG", "error")
        .current_dir("/")
        .args(["put", "shq", &payload, "--path", "/"])
        .assert()
        .success();
}

/// Evaluate `script` in a real `sh` with `COMB_SH_OUT` set to `out`, and
/// return stdout. `HOME` is removed so a `$HOME` that wrongly expanded shows
/// up as a gap rather than silently matching.
fn sh_eval(out: &str, script: &str) -> String {
    let shell = std::process::Command::new("sh")
        .args(["-c", script])
        .env("COMB_SH_OUT", out)
        .env_remove("HOME")
        .output()
        .expect("sh runs");
    String::from_utf8(shell.stdout).unwrap()
}

/// A daemon-backed **scalar** under `-f sh` must be shell-quoted.
#[test]
fn daemon_scalar_sh_is_shell_quoted() {
    let d = TestDaemon::spawn();
    seed(&d);
    assert_eq!(
        daemon_output(&d, "sh", "shq.one"),
        "'it'\\''s a $HOME env'",
        "-f sh must shell-escape a daemon-backed scalar (canon invariant 8)"
    );
}

/// ...and it survives a real `eval` byte-for-byte, the way a shell consumes it:
/// `eval "v=$(comb get -f sh shq.one)"`.
#[test]
fn daemon_scalar_sh_round_trips_through_eval() {
    let d = TestDaemon::spawn();
    seed(&d);
    let out = daemon_output(&d, "sh", "shq.one");
    assert_eq!(
        sh_eval(&out, r#"eval "v=$COMB_SH_OUT"; printf %s "$v""#),
        HOSTILE,
        "the escaped scalar must survive `eval` byte-for-byte"
    );
}

/// A daemon-backed **object** under `-f sh` must escape every value, numbers
/// included, and the whole block must survive `eval "$(...)"`.
#[test]
fn daemon_object_sh_escapes_every_value() {
    let d = TestDaemon::spawn();
    seed(&d);
    let out = daemon_output(&d, "sh", "shq");
    assert_eq!(
        out, "one='it'\\''s a $HOME env'\ntwo='42'",
        "-f sh must shell-escape each object value (canon invariant 8)"
    );
    assert_eq!(
        sh_eval(&out, r#"eval "$COMB_SH_OUT"; printf '%s|%s' "$one" "$two""#),
        format!("{HOSTILE}|42"),
        "the escaped block must survive `eval` byte-for-byte"
    );
}

/// A value containing a newline stays one shell word through `eval`.
#[test]
fn daemon_scalar_sh_escapes_newline() {
    let d = TestDaemon::spawn();
    Command::cargo_bin("comb")
        .unwrap()
        .env("BEACHCOMBER_SOCKET", &d.socket.path)
        .env("RUST_LOG", "error")
        .current_dir("/")
        .args([
            "put",
            "shnl",
            &serde_json::json!({ "v": "line1\nline2" }).to_string(),
            "--path",
            "/",
        ])
        .assert()
        .success();
    let out = daemon_output(&d, "sh", "shnl.v");
    assert_eq!(
        sh_eval(&out, r#"eval "v=$COMB_SH_OUT"; printf %s "$v""#),
        "line1\nline2",
        "a newline inside the value must survive `eval`"
    );
}

/// `-f text` on the daemon-backed path is untouched: values stay raw.
#[test]
fn daemon_text_leaves_values_raw() {
    let d = TestDaemon::spawn();
    seed(&d);
    assert_eq!(daemon_output(&d, "text", "shq.one"), HOSTILE);
    assert_eq!(
        daemon_output(&d, "text", "shq"),
        "one=it's a $HOME env\ntwo=42"
    );
}

/// `-f json` on the daemon-backed path is untouched: the value keeps its
/// natural type and is never shell-quoted.
#[test]
fn daemon_json_is_unescaped_and_typed() {
    let d = TestDaemon::spawn();
    seed(&d);
    let parsed: serde_json::Value =
        serde_json::from_str(&daemon_output(&d, "json", "shq")).expect("json parses");
    assert_eq!(parsed["data"]["one"], serde_json::json!(HOSTILE));
    assert_eq!(parsed["data"]["two"], serde_json::json!(42));
}
