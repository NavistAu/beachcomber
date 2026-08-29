//! End-to-end tests for `comb eval`: env.* injection and daemon-skip behavior.
//!
//! Task 3 deleted the hand-rolled `find_eval_template_pairs` scanner these
//! tests used to exercise directly; reference discovery is now
//! `libbeachcomber::eval::discover_refs`, covered in
//! `libbeachcomber/tests/eval_classify.rs` and `tests/template.rs`.

// ── #2: eval resolves virtual fields (end-to-end via the real binary) ─────────

/// eval resolves a client-side virtual field (rather than querying the daemon for
/// a field that no longer exists after the P1 rename). Uses `aws.profile`
/// (`env.AWS_PROFILE or env.AWS_VAULT or env.AWS_DEFAULT_PROFILE`) — a pure-env
/// virtual cascade with no daemon dependency, so the test needs no daemon and is
/// deterministic on CI. Before the fix, eval sent `aws.profile` to the daemon
/// (where it does not exist) and rendered undefined; now it evaluates the
/// virtual cascade client-side. The bogus socket asserts no daemon is contacted.
#[test]
fn eval_resolves_virtual_field_from_env() {
    let dir = tempfile::TempDir::new().unwrap();
    let bogus_sock = dir.path().join("nonexistent.sock");
    let mut cmd = assert_cmd::Command::cargo_bin("comb").unwrap();
    cmd.env("BEACHCOMBER_SOCKET", &bogus_sock)
        .env("RUST_LOG", "error")
        .env("AWS_PROFILE", "prod-account")
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("cfg"))
        .args(["eval", "{{ aws.profile }}"]);
    cmd.assert()
        .success()
        .stdout(predicates::str::contains("prod-account"));
}

/// A template referencing only env.* never starts/contacts the daemon, even with
/// an unstartable socket path (parent is a regular file).
#[test]
fn eval_env_only_skips_daemon() {
    let dir = tempfile::TempDir::new().unwrap();
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"not a dir").unwrap();
    let unstartable = blocker.join("sock");
    let mut cmd = assert_cmd::Command::cargo_bin("comb").unwrap();
    cmd.env("BEACHCOMBER_SOCKET", &unstartable)
        .env("RUST_LOG", "error")
        .env("FOO", "hi")
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("cfg"))
        .args(["eval", "{{ env.FOO }}"]);
    // No daemon needed → renders despite an unstartable socket.
    cmd.assert()
        .success()
        .stdout(predicates::str::contains("hi"));
}
