//! One form for every diagnostic comb prints, the form clap's own usage
//! errors take: `error: <who>: <why>`, `error: <why>` for a message that
//! already names its subject, and `warning: <why>`.

use crate::common::daemon::TestDaemon;
use std::path::Path;

/// `args` exit 2; returns stderr after checking each line is a diagnostic.
fn diagnostics(d: &TestDaemon, args: &[&str]) -> String {
    let out = d.comb(Path::new("/")).args(args).assert().code(2);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    for line in stderr.lines() {
        assert!(
            line.starts_with("error: ") || line.starts_with("warning: "),
            "comb {args:?}: {line:?} is not an `error:` or `warning:` line\n{stderr}"
        );
    }
    stderr
}

#[test]
fn every_failure_is_an_error_line_naming_its_subject() {
    let d = TestDaemon::spawn();
    let cases: &[(&[&str], &str)] = &[
        (
            &["get", "-f", "json", "--global", "nosuch.x"],
            "error: nosuch.x: no data (cache miss)\n",
        ),
        (&["get", "--global", "a:b:c.x"], "error: a:b:c.x: "),
        (
            &["put", "--global", "rp.x", "{bad"],
            "error: rp.x: invalid JSON: ",
        ),
        (
            &["put", "--global", "--ttl", "0", "rp", "{}"],
            "error: --ttl: must be a whole number of seconds, at least 1s\n",
        ),
        (
            &["status", "--filter", "nosuchcolumn=1"],
            "error: --filter: ",
        ),
        (&["status", "--sort", "nosuchcolumn"], "error: --sort: "),
        (
            &["status", "--tree", "-f", "csv"],
            "error: --tree: works with the human, table and json formats, not csv\n",
        ),
        (&["refresh", "--global", "nosuch"], "error: nosuch: "),
        (
            &["get", "--global", "hostname", "/"],
            "error: --global: cannot be combined with a path\n",
        ),
    ];
    for (args, needle) in cases {
        let stderr = diagnostics(&d, args);
        assert!(
            stderr.contains(needle),
            "comb {args:?}: expected {needle:?} in\n{stderr}"
        );
    }
}

#[test]
fn a_resolver_error_names_its_own_field() {
    let d = TestDaemon::spawn();
    let stderr = diagnostics(&d, &["eval", "{{ 1 | nosuchfilter }}"]);
    assert!(stderr.starts_with("error: <eval>"), "{stderr}");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
}
