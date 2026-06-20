//! Tests for comb eval env.* injection and daemon-skip behavior.
//! These are unit-level tests on run_eval's logic; full e2e in e2e_providers.rs.

// These tests verify the CONTRACT, not the full end-to-end behavior.
// We test the helper functions directly.

use beachcomber::cli::format::find_eval_template_pairs;

#[test]
fn find_eval_template_pairs_finds_multi_ref_cascade_in_block() {
    // Guards against first-ref-only regression for block tags.
    // "{% if git.branch or user.name %}" must yield both pairs.
    let pairs = find_eval_template_pairs("{% if git.branch or user.name %}x{% endif %}");
    assert!(
        pairs.iter().any(|(p, f)| p == "git" && f == "branch"),
        "git.branch missing"
    );
    assert!(
        pairs.iter().any(|(p, f)| p == "user" && f == "name"),
        "user.name missing"
    );
}

#[test]
fn env_refs_in_template_identified() {
    // 'env' must appear as a provider name in template pairs for eval injection.
    let pairs = find_eval_template_pairs("{{ env.MY_VAR }}");
    assert!(
        pairs.iter().any(|(p, f)| p == "env" && f == "MY_VAR"),
        "env.MY_VAR must be found as a pair; got: {pairs:?}"
    );
}

// ── #3: {{ }} expression tags discover ALL refs (not just the first) ──────────

#[test]
fn expr_tag_finds_all_refs_in_cascade() {
    // "{{ env.NOPE or user.name }}" must yield BOTH refs — the first-ref-only
    // scanner missed user.name, so the cascade fell back to an undefined value.
    let pairs = find_eval_template_pairs("{{ env.NOPE or user.name }}");
    assert!(
        pairs.iter().any(|(p, f)| p == "env" && f == "NOPE"),
        "env.NOPE missing; got: {pairs:?}"
    );
    assert!(
        pairs.iter().any(|(p, f)| p == "user" && f == "name"),
        "user.name missing — {{ }} scanner is still first-ref-only; got: {pairs:?}"
    );
}

#[test]
fn expr_tag_nested_ref_records_provider_field_only() {
    // A nested chain records only ("foo","bar") — daemon keys are provider.field
    // (one dot); ".baz" is MiniJinja nested attribute access.
    let pairs = find_eval_template_pairs("{{ foo.bar.baz }}");
    assert!(
        pairs.iter().any(|(p, f)| p == "foo" && f == "bar"),
        "foo.bar missing; got: {pairs:?}"
    );
    assert!(
        !pairs.iter().any(|(p, f)| p == "bar" && f == "baz"),
        "spurious bar.baz recorded; got: {pairs:?}"
    );
}

#[test]
fn expr_tag_skips_string_literal_dotted_pairs() {
    // A dotted string literal must not produce a spurious pair.
    let pairs = find_eval_template_pairs(r#"{{ git.branch or "foo.bar" }}"#);
    assert!(
        pairs.iter().any(|(p, f)| p == "git" && f == "branch"),
        "git.branch missing; got: {pairs:?}"
    );
    assert!(
        !pairs.iter().any(|(p, f)| p == "foo" && f == "bar"),
        "string literal 'foo.bar' must not be a pair; got: {pairs:?}"
    );
}

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
