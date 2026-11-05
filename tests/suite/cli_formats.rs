//! One output-format vocabulary. Every command that prints a value or rows
//! takes `-f` as a format name or a template (recognised by `{{` or `{%`),
//! from the subset of formats it supports. A format suffix on the command
//! (`g.j`, `s.t`, `w.f TPL`) is the short spelling of `-f`, and applies only
//! to a command that has `-f` and that format.

use crate::common::daemon::TestDaemon;
use std::path::Path;

fn ok(d: &TestDaemon, args: &[&str]) -> String {
    let out = d.comb(Path::new("/")).args(args).assert().success();
    String::from_utf8(out.get_output().stdout.clone()).unwrap()
}

/// `args` fail with exit 2 and stderr holding `needle`.
fn refused(d: &TestDaemon, args: &[&str], needle: &str) {
    d.comb(Path::new("/"))
        .args(args)
        .assert()
        .code(2)
        .stderr(predicates::str::contains(needle));
}

fn seed(d: &TestDaemon) {
    ok(
        d,
        &["put", "--global", "fmtprov", r#"{"val":"formatted","n":3}"#],
    );
}

// ── -f takes a format name or a template ─────────────────────────────────────

#[test]
fn get_takes_a_template_as_the_value_of_f() {
    let d = TestDaemon::spawn();
    seed(&d);
    assert_eq!(
        ok(
            &d,
            &["get", "--global", "-f", "[{{ val }}/{{ n }}]", "fmtprov"]
        ),
        "[formatted/3]"
    );
    // The suffix is the short spelling of the same thing.
    assert_eq!(
        ok(&d, &["g.f", "[{{ val }}/{{ n }}]", "fmtprov", "--global"]),
        "[formatted/3]"
    );
}

#[test]
fn status_takes_a_template_through_the_f_suffix() {
    let d = TestDaemon::spawn();
    seed(&d);
    assert_eq!(
        ok(
            &d,
            &[
                "s.f",
                "<{{ field }}>",
                "--filter",
                "provider=fmtprov",
                "--sort",
                "field"
            ]
        ),
        "<n>\n<val>\n"
    );
}

// ── suffixes ─────────────────────────────────────────────────────────────────

#[test]
fn an_unknown_suffix_is_refused() {
    let d = TestDaemon::spawn();
    refused(&d, &["g.Z", "x"], "unknown format suffix '.Z'");
    // `.p` was a second spelling of the default; the default needs none.
    refused(&d, &["g.p", "x"], "unknown format suffix '.p'");
}

// ── help ─────────────────────────────────────────────────────────────────────

#[test]
fn get_and_watch_list_the_same_formats_in_their_help() {
    let d = TestDaemon::spawn();
    let want = "text (default), json, sh, csv, CSV, tsv, TSV, or a template";
    for command in ["get", "watch"] {
        let help = ok(&d, &[command, "--help"]);
        let flat = help.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(flat.contains(want), "{command} --help:\n{help}");
    }
}

/// The `stale` flag is gone, so no help names it, `put --ttl`'s included.
#[test]
fn no_help_mentions_stale() {
    let d = TestDaemon::spawn();
    for command in [
        "daemon", "get", "status", "put", "refresh", "watch", "eval", "init", "check", "kill",
    ] {
        let help = ok(&d, &[command, "--help"]);
        assert!(!help.contains("stale"), "{command} --help:\n{help}");
    }
}

// ── one encoder per format ───────────────────────────────────────────────────

// ── one way to show a path under home ────────────────────────────────────────

// ── a -f template sees what any template sees ────────────────────────────────

/// `args` with `FMT_X=x` in the environment, which must succeed.
fn ok_with_x(d: &TestDaemon, args: &[&str]) -> String {
    let out = d
        .comb(Path::new("/"))
        .env("FMT_X", "x")
        .args(args)
        .assert()
        .success();
    String::from_utf8(out.get_output().stdout.clone()).unwrap()
}

/// The value as `value`, an object's fields by name, and — as in
/// `comb eval` — `env`, provider refs and `comb.*`.
#[test]
fn a_get_template_sees_the_value_env_and_refs() {
    let d = TestDaemon::spawn();
    seed(&d);
    assert_eq!(
        ok_with_x(
            &d,
            &[
                "get",
                "--global",
                "-f",
                "{{ value }}|{{ env.FMT_X }}|{{ fmtprov.n }}|{{ comb.fmtprov.n }}",
                "fmtprov.val"
            ]
        ),
        "formatted|x|3|3"
    );
    assert_eq!(
        ok_with_x(&d, &["g.f", "{{ val }}/{{ n }}", "fmtprov", "--global"]),
        "formatted/3"
    );
}

#[test]
fn a_status_template_sees_the_row_and_env() {
    let d = TestDaemon::spawn();
    seed(&d);
    assert_eq!(
        ok_with_x(
            &d,
            &[
                "s.f",
                "{{ env.FMT_X }}:{{ field }}={{ value }}",
                "--filter",
                "provider=fmtprov",
                "--sort",
                "field"
            ]
        ),
        "x:n=3\nx:val=formatted\n"
    );
}

/// A template that does not compile fails the command once, named, with no
/// engine frame in the message.
#[test]
fn a_template_that_does_not_compile_fails_the_command() {
    let d = TestDaemon::spawn();
    seed(&d);
    for args in [
        &["get", "--global", "-f", "{{ x ", "fmtprov"][..],
        &["status", "-f", "{{ x ", "--filter", "provider=fmtprov"][..],
    ] {
        let out = d.comb(Path::new("/")).args(args).assert().code(2);
        let stderr = String::from_utf8_lossy(&out.get_output().stderr).to_string();
        assert!(stderr.contains("-f: template compile error:"), "{stderr}");
        assert!(!stderr.contains("(in <"), "no engine frame: {stderr}");
    }
}
