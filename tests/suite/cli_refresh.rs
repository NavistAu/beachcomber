//! End-to-end tests for `comb refresh`, the CLI's surface for the daemon's
//! `refresh` op. It re-executes only the Sources the key demands, each at
//! its own scope, and reads nothing back: exit 0 and no output on success,
//! the daemon's own refusal on stderr and exit 2 for a key with no Source
//! behind it. `comb get --force` is the read-and-refresh form; `refresh` is
//! the re-execution alone.
//!
//! The daemon op itself is covered over a raw socket in `tests/suite/refresh.rs`;
//! these tests prove the binary carries a key and a path to it and reports
//! what it answers.

use crate::common::daemon::{TestDaemon, isolate};
use crate::common::git::GitRepoFixture;
use crate::common::poll::poll_until;
use std::path::Path;
use std::time::Duration;

/// `comb get -f json <key>` at `cwd`, parsed. A miss under `json` exits 2, so
/// a key that has gone cold fails here loudly rather than reading as zero.
fn get_json(d: &TestDaemon, cwd: &Path, key: &str) -> serde_json::Value {
    let out = d
        .comb(cwd)
        .args(["get", "-f", "json", key])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "get {key}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// The `age_ms` of a key's `:cache` leaf, in ms. Reading an age never re-executes a Source.
fn age(d: &TestDaemon, cwd: &Path, key: &str) -> u64 {
    let cache_key = key.replacen('.', ":cache.", 1);
    get_json(d, cwd, &cache_key)["age_ms"]
        .as_u64()
        .unwrap_or_else(|| panic!("{cache_key} should carry an age_ms"))
}

#[test]
fn refresh_of_a_provider_prints_nothing_and_exits_0() {
    let d = TestDaemon::spawn();

    d.comb(&d.home)
        .args(["refresh", "hostname"])
        .assert()
        .success()
        .stdout(predicates::str::is_empty())
        .stderr(predicates::str::is_empty());
}

/// A `put`-created namespace has no Source to re-execute: the daemon refuses,
/// and `comb refresh` prints its words alone — the Lua subprocess tier hands
/// that line back as the error message, so nothing may be added to it.
#[test]
fn refresh_of_a_static_namespace_is_the_daemons_refusal() {
    let d = TestDaemon::spawn();

    d.comb(&d.home)
        .args(["put", "my", r#"{"a":1}"#])
        .assert()
        .success();
    d.comb(&d.home)
        .args(["refresh", "my"])
        .assert()
        .code(2)
        .stdout(predicates::str::is_empty())
        .stderr(predicates::str::diff(
            "error: my: nothing to refresh: my has no sources\n",
        ));
}

#[test]
fn refresh_of_a_facet_key_is_an_error() {
    let d = TestDaemon::spawn();

    d.comb(&d.home)
        .args(["refresh", "a:b:c.x"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("invalid facet key"));
}

#[test]
fn refresh_global_is_accepted_for_a_global_source() {
    let d = TestDaemon::spawn();

    d.comb(&d.home)
        .args(["refresh", "--global", "hostname.name"])
        .assert()
        .success()
        .stdout(predicates::str::is_empty())
        .stderr(predicates::str::is_empty());
}

/// `refresh git.lines_added` demands the `diff` Source alone: its record's
/// age falls back to nothing while the `lineage` Source's keeps growing. The
/// scheduler runs the refresh in the background, and ages are real wall
/// time, so the pre-refresh age is first let grow to a value a re-execution
/// cannot reach, and the post-refresh read polls for the fall — the shape
/// `tests/suite/refresh.rs` uses for the same proof over a raw socket.
///
/// The two fields are chosen for what their Sources do *not* do: `head` and
/// `refs` are `Watch` Sources, whose records a read checks against the files
/// they declare (a fixture's, written just now, are racy and run again once),
/// and whose watches a slow host registers late. `diff` and `lineage` are
/// `Poll` Sources: they declare no files and register no watch, so only a
/// refresh writes their records again.
#[test]
fn refresh_of_a_field_re_executes_its_owning_source_alone() {
    let d = TestDaemon::spawn();
    let repo = GitRepoFixture::new();
    let cwd = repo.path();

    // Two field reads: each demands its owning Source and nothing else.
    d.comb(cwd)
        .args(["get", "git.lines_added", "git.branch_default"])
        .assert()
        .success();

    let diff_before = poll_until(
        "git:cache.lines_added age_ms",
        "at least 50ms",
        Duration::from_secs(5),
        Duration::from_millis(5),
        || age(&d, cwd, "git.lines_added"),
        |a| *a >= 50,
    );
    let lineage_before = age(&d, cwd, "git.branch_default");

    d.comb(cwd)
        .args(["refresh", "git.lines_added"])
        .assert()
        .success()
        .stdout(predicates::str::is_empty())
        .stderr(predicates::str::is_empty());

    let diff_after = poll_until(
        "git:cache.lines_added age_ms after refresh git.lines_added",
        "back below its pre-refresh age",
        Duration::from_secs(5),
        Duration::from_millis(5),
        || age(&d, cwd, "git.lines_added"),
        |a| *a < diff_before,
    );
    let lineage_after = age(&d, cwd, "git.branch_default");
    eprintln!(
        "git.lines_added age {diff_before} -> {diff_after}; git.branch_default age {lineage_before} -> {lineage_after}"
    );
    assert!(
        lineage_after >= lineage_before,
        "refresh git.lines_added demands `diff` alone: `lineage` must not have run \
         (branch_default age {lineage_before} -> {lineage_after})"
    );
}

/// A relative path is the caller's: the daemon canonicalises what it receives
/// against *its own* working directory — the test's crate root, a repository
/// of its own — so an unresolved `.` would refresh the wrong repository. The
/// rows the refresh writes name the fixture's path and nothing else.
#[test]
fn refresh_resolves_a_relative_path_against_the_callers_cwd() {
    let d = TestDaemon::spawn();
    let repo = GitRepoFixture::new();
    let cwd = repo.path();
    let canonical = cwd.canonicalize().unwrap();
    let canonical = canonical.to_str().unwrap();

    d.comb(cwd)
        .args(["refresh", "git.lines_added", "."])
        .assert()
        .success();

    let git_paths = || -> Vec<String> {
        let out = d.comb(cwd).args(["status", "-f", "json"]).output().unwrap();
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|row| row["provider"] == "git")
            .map(|row| row["path"].as_str().unwrap_or("null").to_string())
            .collect()
    };
    let paths = poll_until(
        "git rows after `refresh git.lines_added .`",
        "at least one",
        Duration::from_secs(5),
        Duration::from_millis(20),
        git_paths,
        |paths| !paths.is_empty(),
    );
    assert!(
        paths.iter().all(|p| p == canonical),
        "`.` is the caller's cwd, {canonical}: got rows at {paths:?}"
    );
}

/// `--path <dir>` is the positional's other spelling — what the Lua
/// subprocess tier sends — and the three ways of naming the entry exclude
/// each other.
#[test]
fn refresh_takes_the_path_as_a_flag_and_rejects_two_spellings_at_once() {
    let d = TestDaemon::spawn();
    let home = d.home.to_str().unwrap();

    d.comb(&d.home)
        .args(["refresh", "hostname", "--path", home])
        .assert()
        .success()
        .stdout(predicates::str::is_empty())
        .stderr(predicates::str::is_empty());
    // Each rejection is clap's, not the daemon's: a daemon refusal also exits
    // 2, so the conflict text is what proves the two spellings never both
    // reach the wire.
    let rejected: [&[&str]; 3] = [
        &["refresh", "hostname", home, "--path", home],
        &["refresh", "hostname", "--global", "--path", home],
        &["refresh", "hostname", "--global", home],
    ];
    for args in rejected {
        d.comb(&d.home)
            .args(args)
            .assert()
            .code(2)
            .stderr(predicates::str::contains("cannot be used with"));
    }
}

/// The cwd is read only when it is needed, and when it is needed and gone the
/// answer is an error — never a silent refresh of the global entry.
#[test]
fn refresh_from_a_deleted_cwd_with_no_path_is_an_error() {
    let d = TestDaemon::spawn();
    let gone = d.home.join("gone");
    std::fs::create_dir_all(&gone).unwrap();

    // `sh` enters the directory, removes it from under itself, and execs
    // `comb` there; `Command::current_dir` on a missing directory would fail
    // at spawn instead.
    let mut cmd = std::process::Command::new("sh");
    isolate(&mut cmd, &d.home, &d.xdg, &d.socket.path)
        .env("COMB", env!("CARGO_BIN_EXE_comb"))
        .args([
            "-c",
            r#"cd "$1" && rmdir "$1" && exec "$COMB" refresh hostname"#,
            "sh",
        ])
        .arg(&gone);
    assert_cmd::Command::from_std(cmd)
        .assert()
        .code(2)
        .stderr(predicates::str::contains("error: current directory: "));
}
