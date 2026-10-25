//! Virtual fields through `comb get`: the shipped cascades (the managed
//! defaults the daemon seeds) and the cascade mechanics — env terms, cache
//! terms, `or` fall-through, typed results — rendered by the one resolver from
//! the calling shell's environment. Plus the `basename` filter and ref
//! discovery, which are pure library.
//!
//! Every test runs the built `comb` against its own test daemon, whose config
//! dir holds the managed defaults, from a project directory beneath the
//! daemon's temp `$HOME`. Templates a test adds are `put` to `<p>:template`.

use crate::common::daemon::TestDaemon;
use crate::common::socket::IsolatedSocket;
use libbeachcomber::engine::build_pure_engine_env;
use std::path::{Path, PathBuf};

// ── basename filter ──────────────────────────────────────────────────────────

#[test]
fn basename_filter_extracts_last_component() {
    let env = build_pure_engine_env(Default::default());
    let result = env
        .render_str(r#"{{ "/home/user/.venv" | basename }}"#, ())
        .unwrap();
    assert_eq!(result, ".venv");
}

#[test]
fn basename_filter_on_plain_name() {
    let env = build_pure_engine_env(Default::default());
    let result = env.render_str(r#"{{ "myenv" | basename }}"#, ()).unwrap();
    assert_eq!(result, "myenv");
}

#[test]
fn basename_filter_trailing_slash() {
    let env = build_pure_engine_env(Default::default());
    let result = env
        .render_str(r#"{{ "/some/path/" | basename }}"#, ())
        .unwrap();
    // Trailing slash: last non-empty component.
    assert_eq!(result, "path");
}

#[test]
fn basename_filter_empty_string() {
    let env = build_pure_engine_env(Default::default());
    let result = env.render_str(r#"{{ "" | basename }}"#, ()).unwrap();
    assert_eq!(result, "");
}

#[test]
fn basename_filter_follows_posix() {
    let jinja = build_pure_engine_env(Default::default());
    let result = jinja.render_str(r#"{{ "/" | basename }}"#, ()).unwrap();
    assert_eq!(result, "/");
    // Absent in, absent out: nothing to name.
    let result = jinja
        .render_str(r#"[{{ missing | basename | default("none") }}]"#, ())
        .unwrap();
    assert_eq!(result, "[none]");
}

// ── ref discovery ─────────────────────────────────────────────────────────────

#[test]
fn ref_discovery_finds_all_refs_in_cascade() {
    // Guards against first-ref-only regression.
    // "env.A or provider.field or other.field2" must discover ALL three.
    use libbeachcomber::eval::discover_refs;
    use libbeachcomber::virtual_fields::Ref;
    let refs = discover_refs("{{ env.A or provider.field or other.field2 }}");
    assert!(refs.contains(&Ref::Env("A".into())), "env.A missing");
    assert!(
        refs.contains(&Ref::Resolved("provider".into(), "field".into())),
        "provider.field missing"
    );
    assert!(
        refs.contains(&Ref::Resolved("other".into(), "field2".into())),
        "other.field2 missing"
    );
    assert_eq!(refs.len(), 3, "must find exactly 3 refs; got: {refs:?}");
}

#[test]
fn nested_ref_discovery_keeps_every_segment_of_a_resolved_ref() {
    // A ref names a node at any depth, never the group above it:
    // `foo.object.key` is Resolved("foo", "object.key") and the resolver
    // fetches exactly that leaf. Only `env.*` and `comb.*` cut at a fixed
    // depth, where deeper segments are attribute navigation into the value.
    use libbeachcomber::eval::discover_refs;
    use libbeachcomber::virtual_fields::Ref;
    assert_eq!(
        discover_refs("{{ foo.object.key }}"),
        vec![Ref::Resolved("foo".into(), "object.key".into())]
    );
    assert_eq!(
        discover_refs("{{ a.b.c.d or env.X.deeper }}"),
        vec![
            Ref::Env("X".into()),
            Ref::Resolved("a".into(), "b.c.d".into()),
        ]
    );
}

// ── harness ───────────────────────────────────────────────────────────────────

/// Every selector the shipped cascades read. Cleared from the child's
/// environment so the developer's own shell cannot decide a test; a test sets
/// the ones it is about.
const SELECTORS: &[&str] = &[
    "AWS_PROFILE",
    "AWS_VAULT",
    "AWS_DEFAULT_PROFILE",
    "AWS_REGION",
    "AWS_DEFAULT_REGION",
    "CLOUDSDK_CORE_PROJECT",
    "CLOUDSDK_ACTIVE_CONFIG_NAME",
    "CONDA_DEFAULT_ENV",
    "OP_SERVICE_ACCOUNT_TOKEN",
    "PYENV_VERSION",
    "MISE_PYTHON_VERSION",
    "VIRTUAL_ENV",
    "TF_WORKSPACE",
    "TF_X",
];

/// A daemon and the project directory the tests query from.
fn daemon() -> (TestDaemon, PathBuf) {
    daemon_with_home(&[])
}

/// A daemon whose `$HOME` holds `files` — `(relative path, contents)` —
/// before it starts, so a file-reading provider finds them there.
fn daemon_with_home(files: &[(&str, &str)]) -> (TestDaemon, PathBuf) {
    let socket = IsolatedSocket::new();
    let home = socket.dir.path().join("home");
    let xdg = socket.dir.path().join("xdg");
    for (rel, body) in files {
        let path = home.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    // The daemon inherits this process's environment, and either of these
    // would point its provider away from the files just written.
    // SAFETY: nextest runs every test in its own process, so no other thread
    // is reading the environment concurrently.
    unsafe {
        std::env::remove_var("AWS_CONFIG_FILE");
        std::env::remove_var("CLOUDSDK_CONFIG");
    }
    let d = TestDaemon::spawn_in(socket, home, xdg);
    let cwd = d.home.join("ws").join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    (d, cwd)
}

/// A `comb` at `cwd` pointed at `d`, with the selectors cleared and `env`
/// set on top.
fn comb(d: &TestDaemon, cwd: &Path, env: &[(&str, &str)]) -> assert_cmd::Command {
    let mut cmd = d.comb(cwd);
    for var in SELECTORS {
        cmd.env_remove(var);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd
}

fn put(d: &TestDaemon, cwd: &Path, args: &[&str]) {
    comb(d, cwd, &[]).arg("put").args(args).assert().success();
}

/// `comb get <key>` with `env` in the shell, as text.
fn get(d: &TestDaemon, cwd: &Path, env: &[(&str, &str)], key: &str) -> String {
    let out = comb(d, cwd, env).args(["get", key]).assert().success();
    String::from_utf8(out.get_output().stdout.clone()).unwrap()
}

/// `comb get -f json <key>` with `env` in the shell, parsed.
fn get_json(d: &TestDaemon, cwd: &Path, env: &[(&str, &str)], key: &str) -> serde_json::Value {
    let out = comb(d, cwd, env)
        .args(["get", "-f", "json", key])
        .assert()
        .success();
    serde_json::from_slice(&out.get_output().stdout).expect("stdout is JSON")
}

// ── cascade mechanics ─────────────────────────────────────────────────────────

#[test]
fn a_cascade_takes_the_first_non_empty_term() {
    let (d, cwd) = daemon();
    put(
        &d,
        &cwd,
        &["t:template.v", r#""{{ env.TF_X or comb.seed.v }}""#],
    );
    put(&d, &cwd, &["seed", r#"{"v":"staging"}"#]);

    assert_eq!(get(&d, &cwd, &[("TF_X", "dev")], "t.v"), "dev");
}

#[test]
fn a_cascade_falls_through_to_the_cache_when_env_is_empty() {
    let (d, cwd) = daemon();
    put(
        &d,
        &cwd,
        &["t:template.v", r#""{{ env.TF_X or comb.seed.v }}""#],
    );
    put(&d, &cwd, &["seed", r#"{"v":"staging"}"#]);

    assert_eq!(get(&d, &cwd, &[], "t.v"), "staging");
}

/// `key` is absent: a miss, which under text prints nothing and under json
/// prints `null`, and succeeds in both.
fn assert_absent(d: &TestDaemon, cwd: &Path, key: &str) {
    comb(d, cwd, &[])
        .args(["get", key])
        .assert()
        .success()
        .stdout(predicates::str::is_empty())
        .stderr(predicates::str::is_empty());
    comb(d, cwd, &[])
        .args(["get", "-f", "json", key])
        .assert()
        .success()
        .stdout("null\n")
        .stderr(predicates::str::is_empty());
}

#[test]
fn an_all_empty_cascade_is_absent() {
    let (d, cwd) = daemon();
    put(
        &d,
        &cwd,
        &["t:template.v", r#""{{ env.A or env.B or env.C }}""#],
    );

    assert_absent(&d, &cwd, "t.v");
}

#[test]
fn a_large_integer_survives_the_round_trip_as_an_integer() {
    let (d, cwd) = daemon();
    // 2^53 + 1: the first integer an f64 cannot hold, and within the daemon's
    // i64 storage.
    put(&d, &cwd, &["bignum", r#"{"value":9007199254740993}"#]);
    put(&d, &cwd, &["t:template.v", r#""{{ bignum.value }}""#]);

    assert_eq!(
        get_json(&d, &cwd, &[], "t.v"),
        serde_json::json!(9007199254740993u64),
        "a large integer must round-trip as an integer, not a lossy float"
    );
}

#[test]
fn a_ref_walks_into_the_object_it_fetched() {
    let (d, cwd) = daemon();
    put(&d, &cwd, &["infoprov", r#"{"info":{"sub":"val"}}"#]);
    put(&d, &cwd, &["t:template.v", r#""{{ infoprov.info.sub }}""#]);

    assert_eq!(get(&d, &cwd, &[], "t.v"), "val");
}

// ── op.service_account: a bool, never the token ───────────────────────────────

#[test]
fn op_service_account_is_bool_true_when_token_set() {
    let (d, cwd) = daemon();
    let token = "service-account-token-xyz";
    let env = [("OP_SERVICE_ACCOUNT_TOKEN", token)];

    assert_eq!(
        get_json(&d, &cwd, &env, "op.service_account"),
        serde_json::json!(true)
    );
    // Security: the token itself never appears in the output.
    let text = get(&d, &cwd, &env, "op.service_account");
    assert_eq!(text, "true");
    assert!(!text.contains(token));
}

#[test]
fn op_service_account_is_bool_false_when_token_unset() {
    let (d, cwd) = daemon();
    assert_eq!(
        get_json(&d, &cwd, &[], "op.service_account"),
        serde_json::json!(false)
    );
}

// ── terraform.workspace ───────────────────────────────────────────────────────

#[test]
fn terraform_workspace_env_wins() {
    let (d, cwd) = daemon();
    assert_eq!(
        get(&d, &cwd, &[("TF_WORKSPACE", "prod")], "terraform.workspace"),
        "prod"
    );
}

// ── conda.env ─────────────────────────────────────────────────────────────────

#[test]
fn conda_env_returns_conda_default_env_value() {
    let (d, cwd) = daemon();
    assert_eq!(
        get(&d, &cwd, &[("CONDA_DEFAULT_ENV", "myenv")], "conda.env"),
        "myenv"
    );
}

#[test]
fn conda_env_is_absent_when_unset() {
    let (d, cwd) = daemon();
    assert_absent(&d, &cwd, "conda.env");
}

// ── aws.profile ───────────────────────────────────────────────────────────────

#[test]
fn aws_profile_returns_default_when_all_env_unset() {
    let (d, cwd) = daemon();
    assert_eq!(get(&d, &cwd, &[], "aws.profile"), "default");
}

#[test]
fn aws_profile_aws_profile_env_wins() {
    let (d, cwd) = daemon();
    assert_eq!(
        get(&d, &cwd, &[("AWS_PROFILE", "prod")], "aws.profile"),
        "prod"
    );
}

#[test]
fn aws_profile_aws_vault_wins_when_aws_profile_unset() {
    let (d, cwd) = daemon();
    assert_eq!(
        get(&d, &cwd, &[("AWS_VAULT", "prod")], "aws.profile"),
        "prod"
    );
}

// ── aws.region ────────────────────────────────────────────────────────────────

/// A daemon whose `~/.aws/config` holds the profiles
/// `comb.aws_profiles[...]` indexes: the `aws_profiles` provider reads it.
fn aws_daemon() -> (TestDaemon, PathBuf) {
    daemon_with_home(&[(
        ".aws/config",
        "[default]\nregion = us-east-1\n\n[profile staging]\nregion = eu-west-1\n",
    )])
}

#[test]
fn aws_region_env_aws_region_wins() {
    let (d, cwd) = aws_daemon();
    assert_eq!(
        get(&d, &cwd, &[("AWS_REGION", "ap-southeast-1")], "aws.region"),
        "ap-southeast-1"
    );
}

/// Canon: direct env wins over the indexed value. `AWS_DEFAULT_REGION` is set,
/// `AWS_REGION` is not, and the default profile's region must lose.
#[test]
fn aws_region_aws_default_region_beats_profile_index() {
    let (d, cwd) = aws_daemon();
    assert_eq!(
        get(
            &d,
            &cwd,
            &[("AWS_DEFAULT_REGION", "ca-central-1")],
            "aws.region"
        ),
        "ca-central-1"
    );
}

#[test]
fn aws_region_named_profile_via_aws_profile_env() {
    let (d, cwd) = aws_daemon();
    assert_eq!(
        get(&d, &cwd, &[("AWS_PROFILE", "staging")], "aws.region"),
        "eu-west-1"
    );
}

#[test]
fn aws_region_aws_vault_selects_profile_when_aws_profile_unset() {
    let (d, cwd) = aws_daemon();
    assert_eq!(
        get(&d, &cwd, &[("AWS_VAULT", "staging")], "aws.region"),
        "eu-west-1"
    );
}

#[test]
fn aws_region_default_profile_when_no_env() {
    let (d, cwd) = aws_daemon();
    assert_eq!(get(&d, &cwd, &[], "aws.region"), "us-east-1");
}

// ── gcloud.project / gcloud.account ───────────────────────────────────────────

/// A daemon whose `~/.config/gcloud` holds two configurations, `default`
/// active: the `gcloud_configs` provider reads them.
fn gcloud_daemon() -> (TestDaemon, PathBuf) {
    daemon_with_home(&[
        (".config/gcloud/active_config", "default\n"),
        (
            ".config/gcloud/configurations/config_default/properties",
            "[core]\nproject = proj-default\naccount = default@example.com\n",
        ),
        (
            ".config/gcloud/configurations/config_work/properties",
            "[core]\nproject = proj-work\naccount = work@example.com\n",
        ),
    ])
}

#[test]
fn gcloud_project_active_config_from_cache() {
    let (d, cwd) = gcloud_daemon();
    assert_eq!(get(&d, &cwd, &[], "gcloud.project"), "proj-default");
}

#[test]
fn gcloud_account_active_config_from_cache() {
    let (d, cwd) = gcloud_daemon();
    assert_eq!(get(&d, &cwd, &[], "gcloud.account"), "default@example.com");
}

#[test]
fn gcloud_project_cloudsdk_core_project_env_wins() {
    let (d, cwd) = gcloud_daemon();
    assert_eq!(
        get(
            &d,
            &cwd,
            &[("CLOUDSDK_CORE_PROJECT", "explicit-project")],
            "gcloud.project"
        ),
        "explicit-project"
    );
}

#[test]
fn gcloud_project_cloudsdk_active_config_name_overrides_active_config() {
    let (d, cwd) = gcloud_daemon();
    assert_eq!(
        get(
            &d,
            &cwd,
            &[("CLOUDSDK_ACTIVE_CONFIG_NAME", "work")],
            "gcloud.project"
        ),
        "proj-work"
    );
}

#[test]
fn gcloud_account_cloudsdk_active_config_name_overrides_active_config() {
    let (d, cwd) = gcloud_daemon();
    assert_eq!(
        get(
            &d,
            &cwd,
            &[("CLOUDSDK_ACTIVE_CONFIG_NAME", "work")],
            "gcloud.account"
        ),
        "work@example.com"
    );
}

// ── python.version / python.venv_name ─────────────────────────────────────────

#[test]
fn python_version_env_pyenv_version_wins() {
    let (d, cwd) = daemon();
    assert_eq!(
        get(
            &d,
            &cwd,
            &[
                ("PYENV_VERSION", "3.12.0"),
                ("MISE_PYTHON_VERSION", "3.13.0")
            ],
            "python.version"
        ),
        "3.12.0"
    );
}

#[test]
fn python_version_mise_env_when_pyenv_unset() {
    let (d, cwd) = daemon();
    assert_eq!(
        get(
            &d,
            &cwd,
            &[("MISE_PYTHON_VERSION", "3.13.0")],
            "python.version"
        ),
        "3.13.0"
    );
}

#[test]
fn python_venv_name_basename_virtual_env_when_no_local_venv() {
    let (d, cwd) = daemon();
    assert_eq!(
        get(
            &d,
            &cwd,
            &[("VIRTUAL_ENV", "/home/u/.venv")],
            "python.venv_name"
        ),
        ".venv"
    );
}

#[test]
fn python_venv_name_is_absent_without_a_venv() {
    let (d, cwd) = daemon();
    assert_absent(&d, &cwd, "python.venv_name");
}
