//! How comb's own output looks is decided once, from the global `--color`
//! and `--ascii`, a non-empty `NO_COLOR` and whether stdout is a terminal.
//! `--color` wins over `NO_COLOR`, and the template filters (`fg`, `bg`,
//! `bold`, `dim`) follow the same setting.

use crate::common::daemon::{TestDaemon, isolate};
use std::io::BufRead;
use std::path::Path;

const ESC: &str = "\x1b[";

/// `comb args` with `envs` set (an empty value removes the variable): exit
/// code and stdout.
fn run(d: &TestDaemon, args: &[&str], envs: &[(&str, &str)]) -> (i32, String) {
    let mut cmd = d.comb(Path::new("/"));
    cmd.env_remove("NO_COLOR").args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.code() != Some(2),
        "comb {args:?} failed:\n{stderr}"
    );
    (
        out.status.code().unwrap(),
        String::from_utf8(out.stdout).unwrap(),
    )
}

#[test]
fn color_and_ascii_are_global_flags() {
    let d = TestDaemon::spawn();
    for args in [
        &["--color", "always", "status"][..],
        &["status", "--color", "always"],
        &[
            "--color",
            "always",
            "check",
            "--connect-timeout",
            "0",
            "config",
            "--origins",
            "daemon",
        ],
        &[
            "check",
            "--connect-timeout",
            "0",
            "config",
            "--origins",
            "daemon",
            "--color",
            "always",
        ],
    ] {
        let (_, out) = run(&d, args, &[]);
        assert!(out.contains(ESC), "comb {args:?} is coloured:\n{out}");
    }
    let (_, out) = run(&d, &["--ascii", "check", "daemon"], &[]);
    assert!(
        out.lines()
            .any(|l| l.contains("[PASS] beachcomber ") && l.contains(" - pid ")),
        "check's own lines use the ascii glyphs:\n{out}"
    );
}

#[test]
fn the_color_flag_wins_over_no_color() {
    let d = TestDaemon::spawn();
    let (_, out) = run(&d, &["status", "--color", "always"], &[("NO_COLOR", "1")]);
    assert!(out.contains(ESC), "--color always beats NO_COLOR:\n{out}");
    let (_, out) = run(&d, &["status", "--color", "never"], &[]);
    assert!(!out.contains(ESC), "{out}");
}

#[test]
fn the_template_filters_follow_the_color_setting() {
    let d = TestDaemon::spawn();
    let tpl = r##"{{ "x" | fg("#ff0000") }}{{ "y" | bold }}"##;
    let coloured = "\x1b[38;2;255;0;0mx\x1b[0m\x1b[1my\x1b[0m";
    // The colour depth is the caller's terminal: 24-bit says so.
    let run = |d: &TestDaemon, args: &[&str], envs: &[(&str, &str)]| {
        let mut all = vec![("COLORTERM", "truecolor")];
        all.extend_from_slice(envs);
        run(d, args, &all)
    };

    let (_, out) = run(&d, &["eval", "--global", tpl], &[]);
    assert_eq!(
        out, coloured,
        "a pipe is no reason to drop a prompt's colour"
    );
    let (_, out) = run(&d, &["eval", "--global", tpl], &[("NO_COLOR", "1")]);
    assert_eq!(out, "xy");
    let (_, out) = run(&d, &["eval", "--global", tpl, "--color", "never"], &[]);
    assert_eq!(out, "xy");
    let (_, out) = run(
        &d,
        &["eval", "--global", tpl, "--color", "always"],
        &[("NO_COLOR", "1")],
    );
    assert_eq!(out, coloured);
    // An empty NO_COLOR is not set.
    let (_, out) = run(&d, &["eval", "--global", tpl], &[("NO_COLOR", "")]);
    assert_eq!(out, coloured);
    // A bad colour is still an error with colour off.
    let out = d
        .comb(Path::new("/"))
        .args([
            "eval",
            "--global",
            r#"{{ "x" | fg("nope") }}"#,
            "--color",
            "never",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

/// The daemon's stderr is `daemon.log` when it was spawned: no terminal, so
/// no escape codes.
#[test]
fn the_daemon_log_has_no_ansi_without_a_terminal() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let mut child = isolate(
        &mut std::process::Command::new(env!("CARGO_BIN_EXE_comb")),
        tmp.path(),
        tmp.path(),
        &sock,
    )
    .env("RUST_LOG", "info")
    .args(["daemon", "--exit-with-parent", "--socket"])
    .arg(&sock)
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(child.stderr.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let _ = child.kill();
    let _ = child.wait();
    assert!(!line.is_empty(), "the daemon logged nothing");
    assert!(!line.contains(ESC), "{line:?}");
}

/// A colour is drawn at the depth the caller's terminal has: `COLORTERM`, else
/// `TERM`, else 16 colours.
#[test]
fn colours_follow_the_terminal_depth() {
    let d = TestDaemon::spawn();
    let tpl = r##"{{ "x" | fg("#ff0000") }}{{ "y" | bg(236) }}{{ "z" | fg("green") }}"##;
    let eval = |envs: &[(&str, &str)]| {
        let mut all = vec![("COLORTERM", ""), ("TERM", "dumb")];
        all.extend_from_slice(envs);
        run(&d, &["eval", "--global", tpl], &all).1
    };
    assert_eq!(
        eval(&[("COLORTERM", "truecolor")]),
        "\x1b[38;2;255;0;0mx\x1b[0m\x1b[48;5;236my\x1b[0m\x1b[32mz\x1b[0m"
    );
    assert_eq!(
        eval(&[("TERM", "xterm-256color")]),
        "\x1b[38;5;196mx\x1b[0m\x1b[48;5;236my\x1b[0m\x1b[32mz\x1b[0m"
    );
    assert_eq!(
        eval(&[]),
        "\x1b[91mx\x1b[0m\x1b[40my\x1b[0m\x1b[32mz\x1b[0m"
    );
}
