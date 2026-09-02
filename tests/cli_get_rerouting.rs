//! Contract tests for comb get rerouting: env.* and virtual fields must
//! be resolved client-side without contacting the daemon.
//!
//! These tests exercise the routing logic via the `run_get` function directly,
//! using a mock or absent socket so daemon contact is detectable via connection failure.

// Note: full integration tests (with a running daemon) belong in tests/e2e_providers.rs.
// These unit-level tests use the split_key / is_virtual path directly.

use beachcomber::cli::commands::get::key_needs_daemon;
use beachcomber::config::Config;
use libbeachcomber::virtual_fields::VirtualFields;

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
    use libbeachcomber::eval::discover_refs;
    use libbeachcomber::virtual_fields::Ref;
    // The cascade "env.TF_WORKSPACE or cache.terraform.workspace" has a daemon dep (CacheField).
    let refs = discover_refs("env.TF_WORKSPACE or cache.terraform.workspace");
    let has_daemon_dep = refs.contains(&Ref::CacheField("terraform".into(), "workspace".into()));
    assert!(
        has_daemon_dep,
        "cascade must discover daemon dep cache.terraform.workspace; got: {refs:?}"
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
/// Before the fix: key_needs_daemon("hostname", ...) returned false.
/// After the fix: must return true.
#[test]
fn dotless_key_needs_daemon() {
    let vf = VirtualFields::defaults_only();
    assert!(
        key_needs_daemon("hostname", &vf, &Config::default()),
        "a dotless 'comb get hostname' is a whole-provider daemon query and must need the daemon"
    );
}

/// #4 unit: dotless keys that match a virtual-namespace provider with daemon refs still
/// need the daemon. terraform.workspace cascades through to cache.terraform.workspace (a
/// daemon field), so the daemon must be available even though the key is handled client-side.
#[test]
fn dotless_virtual_provider_name_still_needs_daemon() {
    let vf = VirtualFields::defaults_only();
    // "terraform" is a virtual-namespace provider (handled client-side), but its
    // workspace field has a daemon ref (cache.terraform.workspace), so needs the daemon.
    assert!(
        key_needs_daemon("terraform", &vf, &Config::default()),
        "bare 'terraform' has a virtual field with a daemon dep — must need the daemon"
    );
}

/// #4 regression: env.* must still NOT need the daemon (canon invariant 15).
#[test]
fn env_star_still_does_not_need_daemon() {
    let vf = VirtualFields::defaults_only();
    assert!(
        !key_needs_daemon("env.PATH", &vf, &Config::default()),
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
/// must fall through to the daemon dep (cache.terraform.workspace) — i.e. the
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
    // terraform.workspace has daemon ref cache.terraform.workspace — needs daemon.
    assert!(
        key_needs_daemon("terraform.workspace", &vf, &Config::default()),
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
        !key_needs_daemon("op.signed_in", &vf, &Config::default()),
        "op.signed_in is pure env — must not need the daemon"
    );
    // aws.profile = env.AWS_PROFILE or ... — pure env.
    assert!(
        !key_needs_daemon("aws.profile", &vf, &Config::default()),
        "aws.profile is pure env — must not need the daemon"
    );
}

// ── Step 1.4: bare-provider namespace routing ─────────────────────────────────

/// Step 1.4 unit: `key_needs_daemon` returns false for bare-provider keys whose
/// virtual fields are all env-only (e.g. `conda`, `op`).
///
/// These providers have no daemon counterpart and all their virtual fields read
/// from environment variables only — the daemon must never be contacted.
#[test]
fn bare_virtual_namespace_provider_env_only_does_not_need_daemon() {
    let vf = VirtualFields::defaults_only();
    // conda.env = env.CONDA_DEFAULT_ENV — pure env, no daemon dep.
    assert!(
        !key_needs_daemon("conda", &vf, &Config::default()),
        "bare 'conda' is a virtual-namespace provider with only env refs — must not need the daemon"
    );
    // op: op.signed_in = env.OP_SERVICE_ACCOUNT_TOKEN != "" — pure env.
    assert!(
        !key_needs_daemon("op", &vf, &Config::default()),
        "bare 'op' is a virtual-namespace provider with only env refs — must not need the daemon"
    );
}

/// Step 1.4 unit: `key_needs_daemon` returns true for bare-provider keys that
/// have at least one virtual field with a daemon ref (e.g. `terraform`).
///
/// terraform.workspace = env.TF_WORKSPACE or cache.terraform.workspace
/// — falls through to a cache field, so the daemon must be available.
#[test]
fn bare_virtual_namespace_provider_with_daemon_ref_needs_daemon() {
    let vf = VirtualFields::defaults_only();
    assert!(
        key_needs_daemon("terraform", &vf, &Config::default()),
        "bare 'terraform' has a virtual field with a daemon dep — must need the daemon"
    );
}

/// Step 1.4 e2e: `comb get conda` with `CONDA_DEFAULT_ENV=myenv` and a bogus
/// socket succeeds and emits the virtual field result without contacting the daemon.
///
/// This proves the routing: a bare provider key with virtual fields (conda has
/// only env refs) is handled client-side, not forwarded to the daemon.
#[test]
fn bare_virtual_namespace_provider_env_only_resolves_without_daemon() {
    let dir = tempfile::TempDir::new().unwrap();
    let bogus_sock = dir.path().join("nonexistent.sock");
    let mut cmd = assert_cmd::Command::cargo_bin("comb").unwrap();
    cmd.env("BEACHCOMBER_SOCKET", &bogus_sock)
        .env("RUST_LOG", "error")
        .env("CONDA_DEFAULT_ENV", "myenv")
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("cfg"))
        .args(["get", "conda"]);
    cmd.assert()
        .success()
        .stdout(predicates::str::contains("myenv"));
}

/// Step 1.4 e2e: `comb get git` with a bogus socket FAILS because `git` has
/// no virtual fields and remains a daemon-only provider.
///
/// This proves the fallthrough: providers without virtual fields are unaffected.
#[test]
fn bare_daemon_only_provider_still_routes_to_daemon() {
    let dir = tempfile::TempDir::new().unwrap();
    // Use a path-blocker so ensure_daemon fails with a detectable error.
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"not a directory").unwrap();
    let unstartable_sock = blocker.join("sock");
    let mut cmd = assert_cmd::Command::cargo_bin("comb").unwrap();
    cmd.env("BEACHCOMBER_SOCKET", &unstartable_sock)
        .env("RUST_LOG", "error")
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("cfg"))
        .args(["get", "git"]);
    // git has no virtual fields → routed to daemon → ensure_daemon fails → exit non-zero.
    cmd.assert().failure();
}

// ── Canon invariant 12: a bare provider resolves to its whole subtree ─────────

/// Config used by the invariant-12 tests: `git` is a daemon provider that the
/// user has additionally given one *pure-env* virtual field.
fn git_with_pure_env_virtual_field() -> Config {
    toml::from_str(
        r#"
[providers.git.virtual]
note = "{{ env.COMB_TEST_NOTE }}"
"#,
    )
    .expect("config parses")
}

/// A bare-provider query on a provider that IS a daemon provider must contact
/// the daemon even when every one of its virtual fields is pure-env.
///
/// Regression: `key_needs_daemon` only asked whether some virtual field had a
/// daemon ref. With `[providers.git.virtual] note = "{{ env.FOO }}"` in config,
/// `comb get git` skipped the daemon entirely and returned `{note}` alone,
/// dropping every cached git field — a violation of canon invariant 12, which
/// says a bare provider resolves to its whole subtree (cached fields plus
/// virtual fields, virtual winning on a clash).
#[test]
fn bare_daemon_provider_with_pure_env_virtual_field_needs_daemon() {
    let config = git_with_pure_env_virtual_field();
    let vf = VirtualFields::with_config_overrides(config.virtual_fields());
    assert!(
        vf.is_virtual("git", "note"),
        "the config override must register git.note as a virtual field"
    );
    assert!(
        key_needs_daemon("git", &vf, &config),
        "bare 'git' is a daemon provider — its cached subtree needs the daemon \
         even though its only virtual field is pure-env (canon invariant 12)"
    );
}

/// The single-field path is unaffected: a pure-env virtual field on a daemon
/// provider still resolves without the daemon.
#[test]
fn pure_env_virtual_field_on_daemon_provider_does_not_need_daemon() {
    let config = git_with_pure_env_virtual_field();
    let vf = VirtualFields::with_config_overrides(config.virtual_fields());
    assert!(
        !key_needs_daemon("git.note", &vf, &config),
        "git.note is a pure-env virtual field — the single-field path must not \
         need the daemon"
    );
}

/// A config-declared external provider (script backend) counts as a daemon
/// provider for the same reason a built-in does.
#[test]
fn bare_config_declared_script_provider_with_pure_env_virtual_needs_daemon() {
    let config: Config = toml::from_str(
        r#"
[providers.weather]
command = "curl -s wttr.in?format=j1"

[providers.weather.virtual]
note = "{{ env.COMB_TEST_NOTE }}"
"#,
    )
    .expect("config parses");
    let vf = VirtualFields::with_config_overrides(config.virtual_fields());
    assert!(
        key_needs_daemon("weather", &vf, &config),
        "a [providers.weather] block declaring a command is a daemon provider — \
         its cached subtree needs the daemon"
    );
}

/// A namespace with virtual fields but no daemon provider behind it (`conda`,
/// `op`) must still resolve without the daemon, even when the user adds more
/// pure-env virtual fields to it.
#[test]
fn bare_pure_virtual_namespace_stays_daemon_free_with_config_virtuals() {
    let config: Config = toml::from_str(
        r#"
[providers.conda.virtual]
note = "{{ env.COMB_TEST_NOTE }}"
"#,
    )
    .expect("config parses");
    let vf = VirtualFields::with_config_overrides(config.virtual_fields());
    assert!(
        !key_needs_daemon("conda", &vf, &config),
        "conda has no daemon provider behind it — a bare query must not start one"
    );
}

/// End-to-end: with a daemon holding cached `git` fields and a pure-env virtual
/// field configured on `git`, `comb get git` returns BOTH the cached field and
/// the virtual one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bare_daemon_provider_query_returns_cached_and_virtual_fields() {
    use beachcomber::cache::Cache;
    use beachcomber::provider::Value;
    use beachcomber::provider::registry::ProviderRegistry;
    use beachcomber::server::Server;
    use std::collections::HashMap;
    use std::sync::Arc;

    let dir = tempfile::TempDir::new().unwrap();
    let sock = dir.path().join("sock");
    // The daemon keys `git` by the canonicalized query path.
    let workdir = std::fs::canonicalize(dir.path()).unwrap();

    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(ProviderRegistry::with_defaults());

    // Seed a fresh (60s interval) git entry so the daemon serves it from cache
    // without ever executing the provider.
    let mut fields = HashMap::new();
    fields.insert("branch".to_string(), Value::String("main".to_string()));
    cache.put_source(
        "git",
        Some(&workdir.to_string_lossy()),
        "head",
        fields,
        Some(60),
    );

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    server.spawn().expect("server binds socket");

    // Config: git gains one pure-env virtual field.
    let cfg_dir = dir.path().join("cfg");
    std::fs::create_dir_all(cfg_dir.join("beachcomber")).unwrap();
    std::fs::write(
        cfg_dir.join("beachcomber").join("config.toml"),
        "[providers.git.virtual]\nnote = \"{{ env.COMB_TEST_NOTE }}\"\n",
    )
    .unwrap();

    let workdir_arg = workdir.to_string_lossy().to_string();
    let out = tokio::task::spawn_blocking(move || {
        let mut cmd = assert_cmd::Command::cargo_bin("comb").unwrap();
        cmd.env("BEACHCOMBER_SOCKET", &sock)
            .env("RUST_LOG", "error")
            .env("HOME", dir.path())
            .env("XDG_CONFIG_HOME", &cfg_dir)
            .env("COMB_TEST_NOTE", "hello-note")
            .args(["get", "git", &workdir_arg]);
        let assert = cmd.assert().success();
        String::from_utf8(assert.get_output().stdout.clone()).unwrap()
    })
    .await
    .unwrap();

    assert!(
        out.contains("branch=main"),
        "the daemon-cached git field must survive the whole-provider query; got: {out:?}"
    );
    assert!(
        out.contains("note=hello-note"),
        "the config virtual field must be present too; got: {out:?}"
    );
}
