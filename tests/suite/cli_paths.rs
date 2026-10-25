//! One path interface for every command that addresses a directory: `get`,
//! `put`, `refresh`, `watch` and `eval` each take a trailing `[PATH]`,
//! `-p/--path <PATH>` and `--global`, the three spellings conflicting with one
//! another, and each `--help` says so in the same words.
//!
//! Every test runs from a sibling directory of the one it addresses, so a value
//! read at the current directory instead would be a miss.

use crate::common::daemon::TestDaemon;
use crate::common::watch::first_line;
use predicates::prelude::*;
use std::path::{Path, PathBuf};

/// `(project, elsewhere)`: the directory a test addresses, and a sibling it
/// runs from.
fn dirs(d: &TestDaemon) -> (PathBuf, PathBuf) {
    let project = d.home.join("ws").join("project");
    let elsewhere = d.home.join("ws").join("elsewhere");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    (project, elsewhere)
}

fn ok(d: &TestDaemon, cwd: &Path, args: &[&str]) -> String {
    let out = d.comb(cwd).args(args).assert().success();
    String::from_utf8(out.get_output().stdout.clone()).unwrap()
}

fn usage_error(d: &TestDaemon, cwd: &Path, args: &[&str]) {
    d.comb(cwd).args(args).assert().code(2);
}

#[test]
fn put_takes_a_trailing_path_the_flag_and_global() {
    let d = TestDaemon::spawn();
    let (project, elsewhere) = dirs(&d);
    let at = project.to_str().unwrap();

    ok(&d, &elsewhere, &["put", "p.trailing", "1", at]);
    ok(&d, &elsewhere, &["put", "-p", at, "p.short", "2"]);
    ok(&d, &elsewhere, &["put", "--path", at, "p.long", "3"]);
    ok(&d, &elsewhere, &["put", "--global", "p.global", "4"]);

    for (key, want) in [
        ("p.trailing", "1"),
        ("p.short", "2"),
        ("p.long", "3"),
        ("p.global", "4"),
    ] {
        assert_eq!(ok(&d, &elsewhere, &["get", key, at]), want, "{key}");
    }
    assert_eq!(
        ok(&d, &elsewhere, &["get", "p.trailing"]),
        "",
        "written at the project, not here"
    );
}

#[test]
fn get_takes_the_short_flag_too() {
    let d = TestDaemon::spawn();
    let (project, elsewhere) = dirs(&d);
    let at = project.to_str().unwrap();
    ok(&d, &elsewhere, &["put", "p.v", "1", at]);
    assert_eq!(ok(&d, &elsewhere, &["get", "-p", at, "p.v"]), "1");
    assert_eq!(ok(&d, &elsewhere, &["get", "--path", at, "p.v"]), "1");
}

#[test]
fn refresh_takes_the_short_flag() {
    let d = TestDaemon::spawn();
    let (project, elsewhere) = dirs(&d);
    let at = project.to_str().unwrap();
    ok(&d, &elsewhere, &["refresh", "-p", at, "hostname"]);
    ok(&d, &elsewhere, &["refresh", "hostname", at]);
    ok(&d, &elsewhere, &["refresh", "--global", "hostname"]);
}

#[test]
fn watch_takes_the_short_flag() {
    let d = TestDaemon::spawn();
    let (project, elsewhere) = dirs(&d);
    let at = project.to_str().unwrap();
    ok(&d, &elsewhere, &["put", "p.v", "\"seen\"", at]);
    assert_eq!(first_line(&d, &elsewhere, &["-p", at, "p.v"]), "seen");
}

#[test]
fn eval_takes_a_trailing_path_the_flag_and_global() {
    let d = TestDaemon::spawn();
    let (project, elsewhere) = dirs(&d);
    let at = project.to_str().unwrap();
    ok(&d, &elsewhere, &["put", "p.v", "\"here\"", at]);
    ok(&d, &elsewhere, &["put", "--global", "g.v", "\"home\""]);

    assert_eq!(ok(&d, &elsewhere, &["eval", "{{ p.v }}", at]), "here");
    assert_eq!(ok(&d, &elsewhere, &["eval", "-p", at, "{{ p.v }}"]), "here");
    assert_eq!(
        ok(&d, &elsewhere, &["eval", "--path", at, "{{ p.v }}"]),
        "here"
    );
    assert_eq!(
        ok(&d, &elsewhere, &["eval", "{{ p.v }}"]),
        "",
        "the current directory, by default"
    );
    assert_eq!(
        ok(&d, &project, &["eval", "--global", "{{ p.v }}{{ g.v }}"]),
        "home",
        "the global entry, wherever it runs"
    );
}

#[test]
fn the_three_spellings_conflict_on_every_command() {
    let d = TestDaemon::spawn();
    let (project, elsewhere) = dirs(&d);
    let at = project.to_str().unwrap();
    for args in [
        vec!["get", "p.v", "--path", at, "--global"],
        vec!["get", "p.v", at, "--global"],
        vec!["put", "p.v", "1", "--path", at, "--global"],
        vec!["put", "p.v", "1", at, "--global"],
        vec!["put", "p.v", "1", at, "--path", at],
        vec!["refresh", "hostname", "--path", at, "--global"],
        vec!["refresh", "hostname", at, "--path", at],
        vec!["watch", "p.v", "--path", at, "--global"],
        vec!["watch", "p.v", at, "--global"],
        vec!["eval", "{{ 1 }}", "--path", at, "--global"],
        vec!["eval", "{{ 1 }}", at, "--global"],
        vec!["eval", "{{ 1 }}", at, "--path", at],
    ] {
        usage_error(&d, &elsewhere, &args);
    }
}

/// The `--path` and `--global` lines of every command's help, and the
/// `[PATH]` line of every command with fixed positionals, read the same.
#[test]
fn every_command_describes_the_path_in_the_same_words() {
    let d = TestDaemon::spawn();
    let (_, elsewhere) = dirs(&d);
    let help = |cmd: &str| ok(&d, &elsewhere, &[cmd, "--help"]);
    // The description of `needle`: beside it, or, in clap's long layout
    // (a command with a long about), on the line below.
    let line = |text: &str, needle: &str| {
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        let at = lines
            .iter()
            .position(|l| l.starts_with(needle))
            .unwrap_or_else(|| panic!("no {needle:?} line in:\n{text}"));
        match lines[at][needle.len()..].trim() {
            "" => lines.get(at + 1).unwrap_or(&"").to_string(),
            beside => beside.to_string(),
        }
    };

    let get = help("get");
    let path_line = line(&get, "-p, --path <PATH>");
    let global_line = line(&get, "--global");
    for cmd in ["put", "refresh", "watch", "eval"] {
        let text = help(cmd);
        assert_eq!(line(&text, "-p, --path <PATH>"), path_line, "{cmd}");
        assert_eq!(line(&text, "--global"), global_line, "{cmd}");
    }
    let positional = line(&help("put"), "[PATH]");
    for cmd in ["refresh", "watch", "eval"] {
        assert_eq!(line(&help(cmd), "[PATH]"), positional, "{cmd}");
    }
}

/// A template's `read`, `lup` and `exec` run in the directory the read
/// addresses: the one given, else the current directory, and `$HOME` for the
/// global entry. `get`, `eval` and `watch` agree, whichever directory they
/// are run from.
#[test]
fn a_template_reads_files_in_the_directory_it_is_addressed_at() {
    let d = TestDaemon::spawn();
    let (project, elsewhere) = dirs(&d);
    std::fs::write(project.join("marker"), "project\n").unwrap();
    std::fs::write(elsewhere.join("marker"), "elsewhere\n").unwrap();
    std::fs::write(d.home.join("marker"), "home\n").unwrap();
    ok(
        &d,
        &elsewhere,
        &[
            "put",
            "--global",
            "t:template.m",
            r#""{{ 'marker' | read | trim }}""#,
        ],
    );
    let at = project.to_str().unwrap();

    assert_eq!(ok(&d, &elsewhere, &["get", "t.m"]), "elsewhere");
    assert_eq!(ok(&d, &elsewhere, &["get", "t.m", at]), "project");
    assert_eq!(ok(&d, &elsewhere, &["get", "-p", at, "t.m"]), "project");
    assert_eq!(ok(&d, &elsewhere, &["get", "--global", "t.m"]), "home");
    assert_eq!(ok(&d, &elsewhere, &["eval", "{{ t.m }}", at]), "project");
    assert_eq!(
        ok(&d, &elsewhere, &["eval", "--global", "{{ t.m }}"]),
        "home"
    );
    assert_eq!(first_line(&d, &elsewhere, &["t.m", at]), "project");
}

/// One rule on every command: an argument is a directory only when it starts
/// with `/`, `~` or `.`. For `get` a bare word is another key; for the commands
/// whose last argument can only be a directory, it is refused with the fix.
#[test]
fn a_trailing_directory_starts_with_slash_tilde_or_dot_on_every_command() {
    let d = TestDaemon::spawn();
    let (project, _) = dirs(&d);
    let ws = project.parent().unwrap();

    for args in [
        &["put", "p.k", "1", "project"][..],
        &["refresh", "git", "project"][..],
        &["watch", "git.branch", "project"][..],
        &["eval", "{{ 1 }}", "project"][..],
    ] {
        d.comb(ws)
            .args(args)
            .assert()
            .code(2)
            .stderr(predicates::str::contains(
                "project is not a directory: write ./project or --path project",
            ));
    }
    ok(&d, ws, &["put", "p.k", "1", "./project"]);
    assert_eq!(ok(&d, ws, &["get", "p.k", "./project"]), "1");
}

/// `~user` is not expanded anywhere: as a directory it is an error naming
/// it, never a directory called `~bob` under the current one.
#[test]
fn a_tilde_user_directory_is_an_error_on_every_command() {
    let d = TestDaemon::spawn();
    let (_project, cwd) = dirs(&d);
    for args in [
        &["get", "x.v", "--path", "~bob/x"][..],
        &["put", "x.v", "1", "-p", "~bob/x"],
        &["eval", "{{ 1 }}", "~bob/x"],
    ] {
        d.comb(&cwd)
            .args(args)
            .assert()
            .code(2)
            .stderr(predicates::str::contains("~bob/x").and(predicates::str::contains("~user")));
    }
}
