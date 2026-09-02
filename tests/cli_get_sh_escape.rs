//! Canon invariant 8: `--format text`/`sh` stringify a resolved value, and `sh`
//! shell-escapes it. Object output (a whole-provider or object-valued query)
//! must escape each value too, not only scalars.
//!
//! `conda` is the cleanest probe: it is a pure-virtual namespace (no daemon
//! provider behind it), so `comb get conda` renders through the CLI's own
//! object formatter with no socket in play.

use assert_cmd::Command;
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
