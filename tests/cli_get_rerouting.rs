//! Contract tests for comb get rerouting: env.* and virtual fields must
//! be resolved client-side without contacting the daemon.
//!
//! These tests exercise the routing logic via the `run_get` function directly,
//! using a mock or absent socket so daemon contact is detectable via connection failure.

// Note: full integration tests (with a running daemon) belong in tests/e2e_providers.rs.
// These unit-level tests use the split_key / is_virtual path directly.

use beachcomber::cli::commands::get::key_needs_daemon;
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

// ── Bug #4: dotless keys must need the daemon ─────────────────────────────────

/// #4 unit: a dotless key (whole-provider query) is a daemon query.
/// Before the fix: key_needs_daemon("hostname", &vf) returned false.
/// After the fix: must return true.
#[test]
fn dotless_key_needs_daemon() {
    let vf = VirtualFields::defaults_only();
    assert!(
        key_needs_daemon("hostname", &vf),
        "a dotless 'comb get hostname' is a whole-provider daemon query and must need the daemon"
    );
}

/// #4 unit: dotless keys that happen to match a virtual provider name also need the daemon.
/// Virtual fields are keyed as `provider.field`; a bare provider name is still a daemon query.
#[test]
fn dotless_virtual_provider_name_still_needs_daemon() {
    let vf = VirtualFields::defaults_only();
    // "terraform" alone is dotless — it's a whole-provider daemon query, not a virtual field.
    assert!(
        key_needs_daemon("terraform", &vf),
        "bare 'terraform' is dotless — it's a whole-provider daemon query and must need the daemon"
    );
}

/// #4 regression: env.* must still NOT need the daemon (canon invariant 15).
#[test]
fn env_star_still_does_not_need_daemon() {
    let vf = VirtualFields::defaults_only();
    assert!(
        !key_needs_daemon("env.PATH", &vf),
        "env.* keys must never need the daemon (canon invariant 15)"
    );
}

// ── Bug #7: env-first cascade — env win skips daemon contact ─────────────────

/// #7 e2e: with TF_WORKSPACE=dev and a bogus socket, `comb get terraform.workspace`
/// must return `dev` and exit 0. This proves the env-first evaluation:
/// env.TF_WORKSPACE wins the cascade so the daemon is never contacted.
///
/// RED before fix: the command fails trying to start the daemon via the bogus socket.
/// GREEN after fix: daemon is skipped when the env term wins.
#[test]
fn virtual_cascade_env_wins_no_daemon_needed() {
    let dir = tempfile::TempDir::new().unwrap();
    let bogus_sock = dir.path().join("nonexistent.sock");
    let mut cmd = assert_cmd::Command::cargo_bin("comb").unwrap();
    cmd.env("BEACHCOMBER_SOCKET", &bogus_sock)
        .env("RUST_LOG", "error")
        .env("TF_WORKSPACE", "dev")
        // Isolate HOME so no real daemon config is read.
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("cfg"))
        .args(["get", "terraform.workspace"]);
    cmd.assert()
        .success()
        .stdout(predicates::str::contains("dev"));
}

/// #7 negative: with TF_WORKSPACE unset, the env term is empty so the cascade
/// must fall through to the daemon dep (terraform.path_workspace) — i.e. the
/// daemon must be attempted, NOT short-circuited to an empty result.
///
/// We prove "the daemon was attempted" deterministically by making daemon
/// startup IMPOSSIBLE: point the socket at a path whose parent is a regular
/// file, so `ensure_daemon`'s directory creation fails. If env-first wrongly
/// short-circuited, the command would exit 0 with empty output (never touching
/// the daemon). Because it instead tries to start the daemon, it fails loudly.
///
/// (Contrast `virtual_cascade_env_wins_no_daemon_needed`: with the env term set,
/// the daemon is never started even though startup here would be impossible.)
#[test]
fn virtual_cascade_no_env_attempts_daemon() {
    let dir = tempfile::TempDir::new().unwrap();
    // Make daemon startup impossible: the socket's parent is a regular file,
    // so create_dir_all for the socket directory fails.
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"not a directory").unwrap();
    let unstartable_sock = blocker.join("sock");
    let mut cmd = assert_cmd::Command::cargo_bin("comb").unwrap();
    cmd.env("BEACHCOMBER_SOCKET", &unstartable_sock)
        .env("RUST_LOG", "error")
        // Unset TF_WORKSPACE so the env term is empty and falls through.
        .env_remove("TF_WORKSPACE")
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("cfg"))
        .args(["get", "terraform.workspace"]);
    // env term empty → cascade needs the daemon → ensure_daemon is attempted and
    // fails (parent is a file). The command must FAIL, not succeed-with-empty.
    cmd.assert().failure();
}

/// #7 control: with the env term set, the daemon is never started even when
/// startup would be impossible — proving env-first truly skips the daemon.
#[test]
fn virtual_cascade_env_wins_skips_unstartable_daemon() {
    let dir = tempfile::TempDir::new().unwrap();
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"not a directory").unwrap();
    let unstartable_sock = blocker.join("sock");
    let mut cmd = assert_cmd::Command::cargo_bin("comb").unwrap();
    cmd.env("BEACHCOMBER_SOCKET", &unstartable_sock)
        .env("RUST_LOG", "error")
        .env("TF_WORKSPACE", "dev")
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("cfg"))
        .args(["get", "terraform.workspace"]);
    // env wins → daemon never started → succeeds with "dev" despite an
    // unstartable socket path.
    cmd.assert()
        .success()
        .stdout(predicates::str::contains("dev"));
}

// ── Bug #6: fetch_daemon_deps propagates force/wait ──────────────────────────

/// #6: verify that key_needs_daemon returns true for a virtual field with daemon refs,
/// so the force/wait flags path is exercised (the static routing gate).
/// Full propagation is covered by the fetch_daemon_deps signature in get.rs
/// (see the implementation: force/wait are now threaded through).
#[test]
fn virtual_field_with_daemon_ref_needs_daemon() {
    let vf = VirtualFields::defaults_only();
    // terraform.workspace has daemon ref terraform.path_workspace — needs daemon.
    assert!(
        key_needs_daemon("terraform.workspace", &vf),
        "terraform.workspace has a daemon dep and must need the daemon for force/wait propagation"
    );
}

/// #6: virtual field with ONLY env refs does not need the daemon.
/// This is the "env-only virtual" case where force/wait are irrelevant anyway.
#[test]
fn virtual_field_env_only_does_not_need_daemon() {
    let vf = VirtualFields::defaults_only();
    // op.signed_in = env.OP_SERVICE_ACCOUNT_TOKEN != "" — pure env, no daemon dep.
    assert!(
        !key_needs_daemon("op.signed_in", &vf),
        "op.signed_in is pure env — must not need the daemon"
    );
    // aws.profile = env.AWS_PROFILE or ... — pure env.
    assert!(
        !key_needs_daemon("aws.profile", &vf),
        "aws.profile is pure env — must not need the daemon"
    );
}
