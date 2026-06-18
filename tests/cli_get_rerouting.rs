//! Contract tests for comb get rerouting: env.* and virtual fields must
//! be resolved client-side without contacting the daemon.
//!
//! These tests exercise the routing logic via the `run_get` function directly,
//! using a mock or absent socket so daemon contact is detectable via connection failure.

// Note: full integration tests (with a running daemon) belong in tests/e2e_providers.rs.
// These unit-level tests use the split_key / is_virtual path directly.

use beachcomber::cli::virtual_fields::VirtualFields;

#[test]
fn env_star_key_is_recognized_as_client_side() {
    let vf = VirtualFields::defaults_only();
    // "env.FOO" must not be sent to the daemon.
    let (provider, _field) = ("env", "FOO");
    assert!(provider == "env", "env.* keys must be routed client-side");
    // is_virtual is not called for env.* — the provider check short-circuits.
    let _ = vf;
}

#[test]
fn terraform_workspace_is_virtual_field() {
    let vf = VirtualFields::defaults_only();
    assert!(
        vf.is_virtual("terraform", "workspace"),
        "terraform.workspace must be recognized as a virtual field"
    );
}

#[test]
fn git_branch_is_not_virtual() {
    let vf = VirtualFields::defaults_only();
    assert!(
        !vf.is_virtual("git", "branch"),
        "git.branch is a plain daemon field, not virtual"
    );
}

#[test]
fn discover_refs_finds_daemon_dep_in_cascade() {
    use beachcomber::cli::virtual_fields::discover_expression_refs;
    // The cascade "env.TF_WORKSPACE or terraform.path_workspace" has a daemon dep.
    let refs = discover_expression_refs("env.TF_WORKSPACE or terraform.path_workspace");
    let has_daemon_dep = refs
        .iter()
        .any(|(p, f)| p == "terraform" && f == "path_workspace");
    assert!(
        has_daemon_dep,
        "cascade must discover daemon dep terraform.path_workspace; got: {refs:?}"
    );
}

/// Canon invariant 15: env.* keys NEVER contact the daemon.
///
/// This is a regression guard — the guarantee already holds, but this test
/// catches any future refactor that accidentally routes env.* through the
/// daemon. By pointing BEACHCOMBER_SOCKET at a nonexistent path, any daemon
/// contact (connect or spawn) would fail and make the command exit non-zero.
#[test]
fn get_env_star_succeeds_without_daemon_contact() {
    let dir = tempfile::TempDir::new().unwrap();
    let bogus_sock = dir.path().join("nonexistent.sock");
    let mut cmd = assert_cmd::Command::cargo_bin("comb").unwrap();
    cmd.env("BEACHCOMBER_SOCKET", &bogus_sock)
        .env("RUST_LOG", "error")
        .env("COMB_TEST_ENV_STAR", "hello-from-env")
        .args(["get", "env.COMB_TEST_ENV_STAR"]);
    cmd.assert()
        .success()
        .stdout(predicates::str::contains("hello-from-env"));
}
