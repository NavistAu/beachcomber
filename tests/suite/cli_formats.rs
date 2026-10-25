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
fn a_name_that_is_neither_a_format_nor_a_template_is_refused() {
    let d = TestDaemon::spawn();
    seed(&d);
    refused(
        &d,
        &["get", "-f", "fmt", "fmtprov"],
        "get has no 'fmt' format",
    );
    refused(
        &d,
        &["get", "-f", "bogus", "fmtprov"],
        "get has no 'bogus' format",
    );
    refused(
        &d,
        &["status", "-f", "bogus"],
        "status has no 'bogus' format",
    );
    refused(
        &d,
        &["check", "config", "--origins", "-f", "bogus"],
        "check config --origins has no 'bogus' format",
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
fn a_suffix_on_a_command_without_f_is_refused_by_name() {
    let d = TestDaemon::spawn();
    refused(&d, &["p.j", "x", "1"], "comb p takes no format");
    refused(&d, &["k.j"], "comb k takes no format");
}

#[test]
fn an_unknown_suffix_is_refused() {
    let d = TestDaemon::spawn();
    refused(&d, &["g.Z", "x"], "unknown format suffix '.Z'");
    // `.p` was a second spelling of the default; the default needs none.
    refused(&d, &["g.p", "x"], "unknown format suffix '.p'");
}

#[test]
fn the_f_suffix_needs_the_template_after_it() {
    let d = TestDaemon::spawn();
    refused(
        &d,
        &["g.f"],
        "the .f suffix takes the template that follows it",
    );
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
