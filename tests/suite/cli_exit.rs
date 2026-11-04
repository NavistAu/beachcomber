//! What comb's exit status says, the same for every command and every
//! format: `0` done, a miss included; `1` a miss under `--strict`
//! (`BEACHCOMBER_STRICT=1`); `2` an error. The format is applied last and
//! never decides the code.

use crate::common::daemon::TestDaemon;
use std::path::Path;

/// `comb args` with `envs` set: its exit code, stdout and stderr.
fn run(d: &TestDaemon, args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = d.comb(Path::new("/"));
    cmd.args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

fn seed(d: &TestDaemon) {
    let (code, _, err) = run(d, &["put", "--global", "ex", r#"{"a":1}"#], &[]);
    assert_eq!(code, 0, "{err}");
}

#[test]
fn a_miss_exits_zero_in_every_format() {
    let d = TestDaemon::spawn();
    for format in [
        "text",
        "json",
        "sh",
        "csv",
        "CSV",
        "tsv",
        "TSV",
        "[{{ value }}]",
    ] {
        let (code, out, err) = run(&d, &["get", "--global", "-f", format, "nosuch.x"], &[]);
        assert_eq!((code, err.as_str()), (0, ""), "-f {format}: {out:?}");
        if format == "json" {
            assert_eq!(out, "null\n");
        }
    }
    let (code, out, err) = run(&d, &["eval", "--global", "{{ nosuch.x }}"], &[]);
    assert_eq!((code, out.as_str(), err.as_str()), (0, "", ""));
}

#[test]
fn strict_makes_a_miss_exit_one() {
    let d = TestDaemon::spawn();
    seed(&d);
    let miss = ["get", "--global", "-f", "json", "nosuch.x"];

    let (code, out, err) = run(&d, &[&["--strict"][..], &miss].concat(), &[]);
    assert_eq!((code, out.as_str(), err.as_str()), (1, "null\n", ""));
    // A global flag, so it goes after the subcommand too.
    let (code, ..) = run(&d, &[&miss[..], &["--strict"]].concat(), &[]);
    assert_eq!(code, 1);
    let (code, ..) = run(&d, &miss, &[("BEACHCOMBER_STRICT", "1")]);
    assert_eq!(code, 1);
    let (code, ..) = run(&d, &miss, &[("BEACHCOMBER_STRICT", "0")]);
    assert_eq!(code, 0);

    // One key missing of several is a miss; the document is still printed.
    let (code, out, _) = run(
        &d,
        &[
            "--strict", "get", "--global", "-f", "json", "ex.a", "nosuch.x",
        ],
        &[],
    );
    assert_eq!(code, 1);
    let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed, serde_json::json!([1, null]));

    // A hit is a hit.
    let (code, out, _) = run(&d, &["--strict", "get", "--global", "ex.a"], &[]);
    assert_eq!((code, out.as_str()), (0, "1"));

    let (code, ..) = run(&d, &["--strict", "eval", "--global", "{{ nosuch.x }}"], &[]);
    assert_eq!(code, 1);
    let (code, ..) = run(&d, &["--strict", "eval", "--global", "{{ ex.a }}"], &[]);
    assert_eq!(code, 0);
}

#[test]
fn an_error_exits_two_whatever_strict_says() {
    let d = TestDaemon::spawn();
    for strict in [&[][..], &["--strict"]] {
        for format in ["text", "json"] {
            let args = [strict, &["get", "--global", "-f", format, "a:b:c.x"]].concat();
            let (code, _, err) = run(&d, &args, &[]);
            assert_eq!(code, 2, "{args:?}: {err}");
        }
    }
}

/// A daemon that outlives `--timeout` was not stopped: that is an error.
#[test]
fn a_kill_that_times_out_is_an_error() {
    let d = TestDaemon::spawn();
    let socket = d.socket.path.to_str().unwrap().to_string();
    let (code, _, err) = run(&d, &["kill", "--timeout", "0", "--socket", &socket], &[]);
    assert_eq!(code, 2, "{err}");
    assert!(
        err.starts_with("error: daemon: did not exit within 0s"),
        "{err}"
    );
}
