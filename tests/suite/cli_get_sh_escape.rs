//! `-f sh` output is what a shell consumes: `eval "$(comb get -f sh <key>)"`.
//! What it prints is `libbeachcomber::render`'s to test; this is the real `sh`
//! over both kinds of value that reach it.

use crate::common::daemon::TestDaemon;
use std::path::Path;

/// A space, a single quote and a `$`, each of which breaks a bare `eval`.
const HOSTILE: &str = "it's a $HOME env";

/// `eval` `out` in a real `sh`, then run `print`. `HOME` is removed so a `$HOME`
/// that wrongly expanded shows up as a gap rather than silently matching.
fn sh_eval(out: &[u8], print: &str) -> String {
    let shell = std::process::Command::new("sh")
        .args(["-c", &format!(r#"eval "$COMB_SH_OUT"; {print}"#)])
        .env("COMB_SH_OUT", std::str::from_utf8(out).unwrap())
        .env_remove("HOME")
        .output()
        .expect("sh runs");
    String::from_utf8(shell.stdout).unwrap()
}

#[test]
fn sh_output_survives_a_real_eval() {
    let d = TestDaemon::spawn();
    let cwd = Path::new("/");

    // Rendered: `conda` is a namespace the daemon holds only a template for, so
    // this object is assembled entirely from rendered leaves.
    let rendered = d
        .comb(cwd)
        .env("CONDA_DEFAULT_ENV", HOSTILE)
        .args(["get", "-f", "sh", "conda"])
        .assert()
        .success();
    assert_eq!(
        sh_eval(&rendered.get_output().stdout, r#"printf %s "$env""#),
        HOSTILE
    );

    // Cached: a `put`-created provider has no template, so this is the daemon's
    // data as stored. A newline stays inside its one shell word.
    let payload = serde_json::json!({"one": HOSTILE, "nl": "line1\nline2"}).to_string();
    d.comb(cwd)
        .args(["put", "shq", &payload, "--path", "/"])
        .assert()
        .success();
    let cached = d
        .comb(cwd)
        .args(["get", "-f", "sh", "shq"])
        .assert()
        .success();
    assert_eq!(
        sh_eval(
            &cached.get_output().stdout,
            r#"printf '%s|%s' "$one" "$nl""#
        ),
        format!("{HOSTILE}|line1\nline2")
    );
}
