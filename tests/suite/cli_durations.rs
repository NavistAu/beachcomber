//! One duration grammar on every flag that takes one: `put --ttl`,
//! `kill --timeout`, `check --connect-timeout` and `check procs --duration`
//! each take a whole number with an optional unit (`ms`, `s`, `m`, `h`), a bare
//! number being seconds, and refuse anything else in the same words — before a
//! daemon is started.

use crate::common::daemon::isolate;
use std::process::Command;

/// The sentence every refused duration carries.
const GRAMMAR: &str = "a whole number with an optional unit";

/// A `comb` with a throwaway `$HOME`, config and socket, and no daemon.
fn comb(tmp: &tempfile::TempDir) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_comb"));
    isolate(
        &mut cmd,
        tmp.path(),
        tmp.path(),
        &tmp.path().join("no-daemon.sock"),
    );
    cmd
}

#[test]
fn every_duration_flag_takes_the_one_grammar() {
    let tmp = tempfile::tempdir().unwrap();

    let out = comb(&tmp)
        .args(["kill", "--timeout", "5s"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    // `0m` and `2s` were refused by these flags' own parsers; with no daemon
    // each check is one quick attempt.
    for args in [
        &["check", "--connect-timeout", "0m", "daemon"][..],
        &["check", "--connect-timeout", "0", "procs", "-d", "2s"][..],
    ] {
        let out = comb(&tmp).args(args).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!stderr.contains("invalid value"), "{args:?}: {stderr}");
    }
}

#[test]
fn a_bad_duration_is_refused_in_the_same_words_on_every_flag() {
    let tmp = tempfile::tempdir().unwrap();
    for args in [
        &["put", "--ttl", "soon", "k", "1"][..],
        &["kill", "--timeout", "soon"][..],
        &["check", "--connect-timeout", "soon", "daemon"][..],
        &["check", "procs", "-d", "soon"][..],
    ] {
        let out = comb(&tmp).args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(GRAMMAR), "{args:?}: {stderr}");
    }
    assert!(
        !tmp.path().join("no-daemon.sock").exists(),
        "a refused flag starts no daemon"
    );
}

/// A ttl is whole seconds of at least one, refused before a daemon starts.
#[test]
fn a_ttl_the_daemon_cannot_take_is_refused_before_it_starts() {
    let tmp = tempfile::tempdir().unwrap();
    for ttl in ["500ms", "0s"] {
        let out = comb(&tmp)
            .args(["put", "--ttl", ttl, "k", "1"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{ttl}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("ttl must be a whole number of seconds, at least 1s"),
            "{ttl}: {stderr}"
        );
    }
    assert!(!tmp.path().join("no-daemon.sock").exists());
}
