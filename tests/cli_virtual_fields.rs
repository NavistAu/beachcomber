//! Tests for the client-side virtual field evaluator (src/cli/virtual_fields.rs),
//! the env.* resolver, the basename filter, and related format helpers.

use beachcomber::cli::format::build_env;

// ── basename filter ──────────────────────────────────────────────────────────

#[test]
fn basename_filter_extracts_last_component() {
    let env = build_env();
    let result = env
        .render_str(r#"{{ "/home/user/.venv" | basename }}"#, ())
        .unwrap();
    assert_eq!(result, ".venv");
}

#[test]
fn basename_filter_on_plain_name() {
    let env = build_env();
    let result = env.render_str(r#"{{ "myenv" | basename }}"#, ()).unwrap();
    assert_eq!(result, "myenv");
}

#[test]
fn basename_filter_trailing_slash() {
    let env = build_env();
    let result = env
        .render_str(r#"{{ "/some/path/" | basename }}"#, ())
        .unwrap();
    // Trailing slash: last non-empty component.
    assert_eq!(result, "path");
}

#[test]
fn basename_filter_empty_string() {
    let env = build_env();
    let result = env.render_str(r#"{{ "" | basename }}"#, ()).unwrap();
    assert_eq!(result, "");
}

use beachcomber::cli::virtual_fields::{EvalContext, VirtualFields};
use serde_json::json;
use std::collections::HashMap;

// ── env.* resolution ─────────────────────────────────────────────────────────

#[test]
fn env_star_set_returns_value() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = [("MY_VAR".to_string(), "hello".to_string())]
        .into_iter()
        .collect();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &HashMap::new(),
    };
    let result = vf
        .evaluate_expression("env.MY_VAR", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(result, json!("hello"));
}

#[test]
fn env_star_unset_returns_empty_string() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = HashMap::new();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &HashMap::new(),
    };
    let result = vf
        .evaluate_expression("env.NONEXISTENT_VAR", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(result, json!(""));
}

// ── typed output ─────────────────────────────────────────────────────────────

#[test]
fn bool_expression_returns_bool_value() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> =
        [("OP_SERVICE_ACCOUNT_TOKEN".to_string(), "secret".to_string())]
            .into_iter()
            .collect();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &HashMap::new(),
    };
    // op.signed_in expression: env.OP_SERVICE_ACCOUNT_TOKEN != ""
    let result = vf
        .evaluate_expression(
            r#"env.OP_SERVICE_ACCOUNT_TOKEN != """#,
            &ctx,
            &mut Default::default(),
        )
        .unwrap();
    assert_eq!(
        result,
        json!(true),
        "non-empty token → signed_in is bool true"
    );
}

#[test]
fn bool_expression_false_when_token_absent() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = HashMap::new();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &HashMap::new(),
    };
    let result = vf
        .evaluate_expression(
            r#"env.OP_SERVICE_ACCOUNT_TOKEN != """#,
            &ctx,
            &mut Default::default(),
        )
        .unwrap();
    assert_eq!(
        result,
        json!(false),
        "absent token → signed_in is bool false"
    );
}

#[test]
fn op_signed_in_never_returns_token_string() {
    // Security regression guard: op.signed_in must return a bool, not the token.
    let vf = VirtualFields::defaults_only();
    let token = "super-secret-token-value";
    let env_vars: HashMap<String, String> =
        [("OP_SERVICE_ACCOUNT_TOKEN".to_string(), token.to_string())]
            .into_iter()
            .collect();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &HashMap::new(),
    };
    let result = vf
        .evaluate_expression(
            r#"env.OP_SERVICE_ACCOUNT_TOKEN != """#,
            &ctx,
            &mut Default::default(),
        )
        .unwrap();
    assert_ne!(
        result.as_str().unwrap_or(""),
        token,
        "result must not be the token string — security regression"
    );
    assert!(result.is_boolean(), "result must be a bool");
}

// ── cascade / all-falsy ───────────────────────────────────────────────────────

#[test]
fn cascade_first_non_empty_wins() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = [("TF_WORKSPACE".to_string(), "dev".to_string())]
        .into_iter()
        .collect();
    let mut daemon_data: HashMap<String, serde_json::Value> = HashMap::new();
    daemon_data.insert("terraform.workspace".to_string(), json!("staging"));
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &daemon_data,
    };
    let result = vf
        .evaluate_expression(
            "env.TF_WORKSPACE or cache.terraform.workspace",
            &ctx,
            &mut Default::default(),
        )
        .unwrap();
    assert_eq!(result, json!("dev"), "env var wins when set");
}

#[test]
fn cascade_falls_through_to_daemon_when_env_empty() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = HashMap::new();
    let mut daemon_data: HashMap<String, serde_json::Value> = HashMap::new();
    daemon_data.insert("terraform.workspace".to_string(), json!("staging"));
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &daemon_data,
    };
    let result = vf
        .evaluate_expression(
            "env.TF_WORKSPACE or cache.terraform.workspace",
            &ctx,
            &mut Default::default(),
        )
        .unwrap();
    assert_eq!(result, json!("staging"), "daemon value used when env empty");
}

#[test]
fn all_falsy_cascade_returns_empty_string() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = HashMap::new();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &HashMap::new(),
    };
    let result = vf
        .evaluate_expression("env.A or env.B or env.C", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(result, json!(""), "all falsy → empty string");
}

// ── ref discovery ─────────────────────────────────────────────────────────────

#[test]
fn ref_discovery_finds_all_refs_in_cascade() {
    // Guards against first-ref-only regression.
    // "env.A or provider.field or other.field2" must discover ALL three.
    use beachcomber::cli::virtual_fields::{Ref, discover_expression_refs};
    let refs = discover_expression_refs("env.A or provider.field or other.field2");
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

// ── large u64 integer preservation ───────────────────────────────────────────

#[test]
fn large_u64_daemon_field_preserved_as_integer() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = HashMap::new();
    let mut daemon_data: HashMap<String, serde_json::Value> = HashMap::new();
    daemon_data.insert("bignum.value".to_string(), json!(18446744073709551615u64)); // u64::MAX
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &daemon_data,
    };
    let result = vf
        .evaluate_expression("bignum.value", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!(18446744073709551615u64),
        "u64::MAX must round-trip as an integer, not a lossy float; got: {result}"
    );
}

// ── is_virtual / built-in defaults ──────────────────────────────────────────

#[test]
fn built_in_defaults_present_without_config() {
    // Built-in defaults must be available with no config file.
    let vf = VirtualFields::defaults_only();
    assert!(
        vf.is_virtual("terraform", "workspace"),
        "terraform.workspace must be a built-in virtual field"
    );
    assert!(
        vf.is_virtual("python", "version"),
        "python.version must be a built-in virtual field"
    );
    assert!(
        vf.is_virtual("conda", "env"),
        "conda.env must be a built-in virtual field"
    );
    assert!(
        vf.is_virtual("aws", "profile"),
        "aws.profile must be a built-in virtual field"
    );
    assert!(
        vf.is_virtual("aws", "region"),
        "aws.region must be a built-in virtual field"
    );
    assert!(
        vf.is_virtual("op", "signed_in"),
        "op.signed_in must be a built-in virtual field"
    );
}

// ── comb init --write-config (binary integration) ────────────────────────────

#[test]
fn init_write_config_is_idempotent_and_valid() {
    use assert_cmd::Command;
    use tempfile::TempDir;

    let tmp = TempDir::new().expect("tempdir");
    let tmp_path = tmp.path();

    // Run once — must succeed.
    Command::cargo_bin("comb")
        .unwrap()
        .args(["init", "--write-config"])
        .env("XDG_CONFIG_HOME", tmp_path)
        .env("HOME", tmp_path)
        .env("RUST_LOG", "error")
        .assert()
        .success();

    // Config file must exist and parse as valid TOML, containing "workspace".
    let config_path = tmp_path.join("beachcomber").join("config.toml");
    let contents =
        std::fs::read_to_string(&config_path).expect("config.toml must exist after first run");
    toml::from_str::<toml::Value>(&contents)
        .expect("config.toml must be valid TOML after first run");
    assert!(
        contents.contains("workspace"),
        "config.toml must contain 'workspace' after first run"
    );

    // Run a SECOND time with the same env — must also succeed.
    Command::cargo_bin("comb")
        .unwrap()
        .args(["init", "--write-config"])
        .env("XDG_CONFIG_HOME", tmp_path)
        .env("HOME", tmp_path)
        .env("RUST_LOG", "error")
        .assert()
        .success();

    // Key regression assertion: file must STILL be valid TOML after second run.
    // On unfixed code, duplicate [providers.*] table headers make it invalid.
    let contents2 = std::fs::read_to_string(&config_path)
        .expect("config.toml must still exist after second run");
    toml::from_str::<toml::Value>(&contents2)
        .expect("config.toml must remain valid TOML after second run (idempotency regression)");
}

// ── gcloud.project cascade (self-cycle regression) ───────────────────────────

#[test]
fn gcloud_project_cascade_no_self_cycle() {
    // Regression: gcloud.project virtual field must NOT cycle into itself.
    // Expression: env.CLOUDSDK_CORE_PROJECT or cache.gcloud_configs[...].project
    // The cache.* form avoids any self-reference.
    let vf = VirtualFields::defaults_only();

    // Case 1: env var set → must return env value, not a cycle error.
    {
        let env_vars: HashMap<String, String> =
            [("CLOUDSDK_CORE_PROJECT".to_string(), "myproj".to_string())]
                .into_iter()
                .collect();
        let ctx = EvalContext {
            env_vars: &env_vars,
            daemon_data: &HashMap::new(),
        };
        let result = vf.evaluate("gcloud", "project", &ctx, &mut Default::default());
        assert!(
            result.is_ok(),
            "env var set: expected Ok, got cycle error: {result:?}"
        );
        assert_eq!(result.unwrap(), json!("myproj"), "env var must win");
    }

    // Case 2: env unset + daemon has gcloud_configs → must return project from active config.
    {
        let env_vars: HashMap<String, String> = HashMap::new();
        let mut daemon_data: HashMap<String, serde_json::Value> = HashMap::new();
        // cache.gcloud_configs is a CacheProvider ref — key is "gcloud_configs" (no dot).
        // cache.gcloud_configs.active_config is a CacheField ref — key is "gcloud_configs.active_config".
        // Both are needed: the CacheProvider for indexing, the CacheField for active_config lookup.
        let configs_obj = json!({"active_config": "default", "default": {"project": "fromfile", "account": "user@example.com"}});
        daemon_data.insert("gcloud_configs".to_string(), configs_obj.clone());
        daemon_data.insert("gcloud_configs.active_config".to_string(), json!("default"));
        let ctx = EvalContext {
            env_vars: &env_vars,
            daemon_data: &daemon_data,
        };
        let result = vf.evaluate("gcloud", "project", &ctx, &mut Default::default());
        assert!(
            result.is_ok(),
            "daemon fallback: expected Ok, got cycle error: {result:?}"
        );
        assert_eq!(
            result.unwrap(),
            json!("fromfile"),
            "daemon value must be used when env unset"
        );
    }
}

// ── nested ref discovery ──────────────────────────────────────────────────────

#[test]
fn nested_ref_discovery_three_segments_returns_provider_and_second_segment() {
    // foo.object.key must yield Resolved("foo", "object") — deeper segments are MiniJinja
    // attribute navigation into the fetched value, not part of the daemon key.
    use beachcomber::cli::virtual_fields::{Ref, discover_expression_refs};
    let refs = discover_expression_refs("foo.object.key");
    assert_eq!(refs.len(), 1, "one ref expected; got: {refs:?}");
    assert!(
        refs.contains(&Ref::Resolved("foo".into(), "object".into())),
        "expected Resolved(\"foo\", \"object\"); got: {refs:?}"
    );
}

#[test]
fn nested_ref_discovery_four_segments_and_two_segment_ref() {
    // "a.b.c.d or env.X" → [Resolved("a", "b"), Env("X")]
    use beachcomber::cli::virtual_fields::{Ref, discover_expression_refs};
    let refs = discover_expression_refs("a.b.c.d or env.X");
    assert_eq!(refs.len(), 2, "two refs expected; got: {refs:?}");
    assert!(
        refs.contains(&Ref::Resolved("a".into(), "b".into())),
        "expected Resolved(\"a\", \"b\"); got: {refs:?}"
    );
    assert!(
        refs.contains(&Ref::Env("X".into())),
        "expected Env(\"X\"); got: {refs:?}"
    );
}

#[test]
fn nested_ref_evaluates_via_minijinja_attribute_navigation() {
    // With daemon_data containing key "git.info" = {"sub": "val"},
    // expression "git.info.sub" must resolve to json!("val") via MiniJinja
    // navigating into the fetched object value.
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = HashMap::new();
    let mut daemon_data: HashMap<String, serde_json::Value> = HashMap::new();
    daemon_data.insert("git.info".to_string(), json!({"sub": "val"}));
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &daemon_data,
    };
    let result = vf
        .evaluate_expression("git.info.sub", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!("val"),
        "nested attribute navigation must resolve to inner value; got: {result}"
    );
}

// ── to_config_toml ────────────────────────────────────────────────────────────

#[test]
fn materialize_defaults_produces_valid_toml() {
    // Confirms that the built-in defaults can be serialized to a valid TOML snippet.
    let vf = VirtualFields::defaults_only();
    let toml_str = vf.to_config_toml();
    // Must be parseable TOML.
    toml::from_str::<toml::Value>(&toml_str).expect("materialized defaults must be valid TOML");
    // Must contain at least the terraform.workspace entry.
    assert!(
        toml_str.contains("workspace"),
        "materialized TOML must contain 'workspace'; got:\n{toml_str}"
    );
}

// ── Per-cascade behavior tests (Task 5) ──────────────────────────────────────

// Helper: build daemon_data with aws_profiles whole-object (CacheProvider key "aws_profiles").
fn aws_profiles_daemon_data() -> HashMap<String, serde_json::Value> {
    let mut d = HashMap::new();
    d.insert(
        "aws_profiles".to_string(),
        json!({"default": {"region": "us-east-1"}, "staging": {"region": "eu-west-1"}}),
    );
    d
}

// Helper: build daemon_data with gcloud_configs whole-object plus active_config CacheField.
fn gcloud_configs_daemon_data(active: &str) -> HashMap<String, serde_json::Value> {
    let mut d = HashMap::new();
    let obj = json!({
        "active_config": active,
        "default": {"project": "proj-default", "account": "default@example.com"},
        "work": {"project": "proj-work", "account": "work@example.com"}
    });
    d.insert("gcloud_configs".to_string(), obj);
    d.insert("gcloud_configs.active_config".to_string(), json!(active));
    d
}

// ── aws.region cascade ────────────────────────────────────────────────────────

#[test]
fn aws_region_env_aws_region_wins() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> =
        [("AWS_REGION".to_string(), "ap-southeast-1".to_string())]
            .into_iter()
            .collect();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &aws_profiles_daemon_data(),
    };
    let result = vf
        .evaluate("aws", "region", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!("ap-southeast-1"),
        "AWS_REGION env var must win"
    );
}

#[test]
fn aws_region_named_profile_via_aws_profile_env() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = [("AWS_PROFILE".to_string(), "staging".to_string())]
        .into_iter()
        .collect();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &aws_profiles_daemon_data(),
    };
    let result = vf
        .evaluate("aws", "region", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!("eu-west-1"),
        "AWS_PROFILE=staging should select staging profile region"
    );
}

#[test]
fn aws_region_default_profile_when_no_env() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = HashMap::new();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &aws_profiles_daemon_data(),
    };
    let result = vf
        .evaluate("aws", "region", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!("us-east-1"),
        "unset AWS_PROFILE must fall back to 'default' profile region"
    );
}

// ── aws.profile cascade ───────────────────────────────────────────────────────

#[test]
fn aws_profile_returns_default_when_all_env_unset() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = HashMap::new();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &HashMap::new(),
    };
    let result = vf
        .evaluate("aws", "profile", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!("default"),
        "aws.profile must return \"default\" when all env vars unset"
    );
}

#[test]
fn aws_profile_aws_profile_env_wins() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = [("AWS_PROFILE".to_string(), "prod".to_string())]
        .into_iter()
        .collect();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &HashMap::new(),
    };
    let result = vf
        .evaluate("aws", "profile", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(result, json!("prod"), "AWS_PROFILE env var must win");
}

// ── gcloud.project cascade ────────────────────────────────────────────────────

#[test]
fn gcloud_project_active_config_from_cache() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = HashMap::new();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &gcloud_configs_daemon_data("default"),
    };
    let result = vf
        .evaluate("gcloud", "project", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!("proj-default"),
        "gcloud.project must use active_config from cache"
    );
}

#[test]
fn gcloud_project_cloudsdk_active_config_name_env_overrides() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = [(
        "CLOUDSDK_ACTIVE_CONFIG_NAME".to_string(),
        "work".to_string(),
    )]
    .into_iter()
    .collect();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &gcloud_configs_daemon_data("default"),
    };
    let result = vf
        .evaluate("gcloud", "project", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!("proj-work"),
        "CLOUDSDK_ACTIVE_CONFIG_NAME env var must override cache.gcloud_configs.active_config"
    );
}

#[test]
fn gcloud_project_cloudsdk_core_project_env_wins() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = [(
        "CLOUDSDK_CORE_PROJECT".to_string(),
        "explicit-project".to_string(),
    )]
    .into_iter()
    .collect();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &gcloud_configs_daemon_data("default"),
    };
    let result = vf
        .evaluate("gcloud", "project", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!("explicit-project"),
        "CLOUDSDK_CORE_PROJECT env var must win over cache"
    );
}

// ── gcloud.account cascade ────────────────────────────────────────────────────

#[test]
fn gcloud_account_active_config_from_cache() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = HashMap::new();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &gcloud_configs_daemon_data("work"),
    };
    let result = vf
        .evaluate("gcloud", "account", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!("work@example.com"),
        "gcloud.account must use active_config from cache"
    );
}

#[test]
fn gcloud_account_cloudsdk_active_config_name_env_overrides() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = [(
        "CLOUDSDK_ACTIVE_CONFIG_NAME".to_string(),
        "work".to_string(),
    )]
    .into_iter()
    .collect();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &gcloud_configs_daemon_data("default"),
    };
    let result = vf
        .evaluate("gcloud", "account", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!("work@example.com"),
        "CLOUDSDK_ACTIVE_CONFIG_NAME must select the work config account"
    );
}

// ── python.version cascade ────────────────────────────────────────────────────

#[test]
fn python_version_env_pyenv_version_wins() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = [("PYENV_VERSION".to_string(), "3.12.0".to_string())]
        .into_iter()
        .collect();
    let mut daemon_data: HashMap<String, serde_json::Value> = HashMap::new();
    daemon_data.insert("mise.python".to_string(), json!("3.11.0"));
    daemon_data.insert("python.venv_version".to_string(), json!("3.10.0"));
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &daemon_data,
    };
    let result = vf
        .evaluate("python", "version", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!("3.12.0"),
        "PYENV_VERSION must win over cache.mise.python and cache.python.venv_version"
    );
}

#[test]
fn python_version_cache_mise_python_fallback() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = HashMap::new();
    let mut daemon_data: HashMap<String, serde_json::Value> = HashMap::new();
    daemon_data.insert("mise.python".to_string(), json!("3.11.5"));
    daemon_data.insert("python.venv_version".to_string(), json!("3.10.0"));
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &daemon_data,
    };
    let result = vf
        .evaluate("python", "version", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!("3.11.5"),
        "cache.mise.python must win over cache.python.venv_version"
    );
}

#[test]
fn python_version_cache_python_venv_version_fallback() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = HashMap::new();
    let mut daemon_data: HashMap<String, serde_json::Value> = HashMap::new();
    daemon_data.insert("python.venv_version".to_string(), json!("3.10.14"));
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &daemon_data,
    };
    let result = vf
        .evaluate("python", "version", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!("3.10.14"),
        "cache.python.venv_version must be used when mise/asdf not available"
    );
}

// ── terraform.workspace cascade ───────────────────────────────────────────────

#[test]
fn terraform_workspace_env_wins() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = [("TF_WORKSPACE".to_string(), "prod".to_string())]
        .into_iter()
        .collect();
    let mut daemon_data: HashMap<String, serde_json::Value> = HashMap::new();
    daemon_data.insert("terraform.workspace".to_string(), json!("staging"));
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &daemon_data,
    };
    let result = vf
        .evaluate("terraform", "workspace", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(result, json!("prod"), "TF_WORKSPACE env var must win");
}

#[test]
fn terraform_workspace_cache_fallback() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = HashMap::new();
    let mut daemon_data: HashMap<String, serde_json::Value> = HashMap::new();
    daemon_data.insert("terraform.workspace".to_string(), json!("main"));
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &daemon_data,
    };
    let result = vf
        .evaluate("terraform", "workspace", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!("main"),
        "cache.terraform.workspace must be used when TF_WORKSPACE unset"
    );
}

// ── op.signed_in security guard ───────────────────────────────────────────────

#[test]
fn op_signed_in_is_bool_true_when_token_set() {
    let vf = VirtualFields::defaults_only();
    let token = "service-account-token-xyz";
    let env_vars: HashMap<String, String> =
        [("OP_SERVICE_ACCOUNT_TOKEN".to_string(), token.to_string())]
            .into_iter()
            .collect();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &HashMap::new(),
    };
    let result = vf
        .evaluate("op", "signed_in", &ctx, &mut Default::default())
        .unwrap();
    assert!(
        result.is_boolean(),
        "op.signed_in must be a bool, not a string"
    );
    assert_eq!(
        result,
        json!(true),
        "op.signed_in must be true when token set"
    );
    // Security: must NEVER contain the token string.
    assert_ne!(
        result.as_str().unwrap_or(""),
        token,
        "op.signed_in must not leak the token value"
    );
}

#[test]
fn op_signed_in_is_bool_false_when_token_unset() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = HashMap::new();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &HashMap::new(),
    };
    let result = vf
        .evaluate("op", "signed_in", &ctx, &mut Default::default())
        .unwrap();
    assert!(
        result.is_boolean(),
        "op.signed_in must be a bool, not a string"
    );
    assert_eq!(
        result,
        json!(false),
        "op.signed_in must be false when token unset"
    );
}

// ── aws.region: AWS_DEFAULT_REGION beats profile index ───────────────────────

/// Canon: direct env wins over indexed value.
/// AWS_DEFAULT_REGION is set; AWS_REGION is not.
/// The cascade is: AWS_REGION or AWS_DEFAULT_REGION or cache index.
/// Result must be AWS_DEFAULT_REGION's value, not the indexed profile region.
#[test]
fn aws_region_aws_default_region_beats_profile_index() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> =
        [("AWS_DEFAULT_REGION".to_string(), "ca-central-1".to_string())]
            .into_iter()
            .collect();
    // Provide aws_profiles data — it must NOT be selected because env wins.
    let daemon: HashMap<String, serde_json::Value> = [(
        "aws_profiles".to_string(),
        json!({"default": {"region": "us-east-1"}}),
    )]
    .into_iter()
    .collect();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &daemon,
    };
    let result = vf
        .evaluate("aws", "region", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!("ca-central-1"),
        "AWS_DEFAULT_REGION must win over the profile-indexed region; got: {result}"
    );
}

// ── aws.region / aws.profile: AWS_VAULT selects profile when AWS_PROFILE unset

/// When AWS_PROFILE is unset, AWS_VAULT is the fallback profile selector.
#[test]
fn aws_region_aws_vault_selects_profile_when_aws_profile_unset() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = [("AWS_VAULT".to_string(), "staging".to_string())]
        .into_iter()
        .collect();
    let daemon: HashMap<String, serde_json::Value> = [(
        "aws_profiles".to_string(),
        json!({"default": {"region": "us-east-1"}, "staging": {"region": "eu-central-1"}}),
    )]
    .into_iter()
    .collect();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &daemon,
    };
    let result = vf
        .evaluate("aws", "region", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!("eu-central-1"),
        "AWS_VAULT must select the staging profile region when AWS_PROFILE is unset; got: {result}"
    );
}

#[test]
fn aws_profile_aws_vault_wins_when_aws_profile_unset() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = [("AWS_VAULT".to_string(), "prod".to_string())]
        .into_iter()
        .collect();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &HashMap::new(),
    };
    let result = vf
        .evaluate("aws", "profile", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!("prod"),
        "AWS_VAULT must be used as profile name when AWS_PROFILE is unset; got: {result}"
    );
}

// ── python.version: MISE_PYTHON_VERSION beats cache ──────────────────────────

#[test]
fn python_version_mise_env_beats_cache() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> =
        [("MISE_PYTHON_VERSION".to_string(), "3.13.0".to_string())]
            .into_iter()
            .collect();
    let mut daemon_data: HashMap<String, serde_json::Value> = HashMap::new();
    daemon_data.insert("asdf.python".to_string(), json!("3.11.0"));
    daemon_data.insert("python.venv_version".to_string(), json!("3.10.0"));
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &daemon_data,
    };
    let result = vf
        .evaluate("python", "version", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!("3.13.0"),
        "MISE_PYTHON_VERSION env must beat cache.asdf.python and cache.python.venv_version; got: {result}"
    );
}

/// cache.asdf.python beats cache.python.venv_version when mise env is empty.
#[test]
fn python_version_cache_asdf_python_beats_venv_version() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = HashMap::new();
    let mut daemon_data: HashMap<String, serde_json::Value> = HashMap::new();
    daemon_data.insert("asdf.python".to_string(), json!("3.11.9"));
    daemon_data.insert("python.venv_version".to_string(), json!("3.10.0"));
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &daemon_data,
    };
    let result = vf
        .evaluate("python", "version", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!("3.11.9"),
        "cache.asdf.python must beat cache.python.venv_version when mise env is empty; got: {result}"
    );
}

// ── python.venv_name: cache wins; basename(VIRTUAL_ENV) as fallback ──────────

#[test]
fn python_venv_name_cache_local_venv_name_wins() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = [(
        "VIRTUAL_ENV".to_string(),
        "/home/u/.some-other-venv".to_string(),
    )]
    .into_iter()
    .collect();
    let mut daemon_data: HashMap<String, serde_json::Value> = HashMap::new();
    daemon_data.insert("python.local_venv_name".to_string(), json!("project-venv"));
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &daemon_data,
    };
    let result = vf
        .evaluate("python", "venv_name", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!("project-venv"),
        "cache.python.local_venv_name must win over basename(VIRTUAL_ENV); got: {result}"
    );
}

#[test]
fn python_venv_name_basename_virtual_env_when_cache_empty() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> =
        [("VIRTUAL_ENV".to_string(), "/home/u/.venv".to_string())]
            .into_iter()
            .collect();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &HashMap::new(),
    };
    let result = vf
        .evaluate("python", "venv_name", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!(".venv"),
        "basename(VIRTUAL_ENV) must be used when cache.python.local_venv_name is absent; got: {result}"
    );
}

// ── conda.env: CONDA_DEFAULT_ENV → value; unset → "" ─────────────────────────

#[test]
fn conda_env_returns_conda_default_env_value() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> =
        [("CONDA_DEFAULT_ENV".to_string(), "myenv".to_string())]
            .into_iter()
            .collect();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &HashMap::new(),
    };
    let result = vf
        .evaluate("conda", "env", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!("myenv"),
        "conda.env must return CONDA_DEFAULT_ENV value; got: {result}"
    );
}

#[test]
fn conda_env_returns_empty_string_when_unset() {
    let vf = VirtualFields::defaults_only();
    let env_vars: HashMap<String, String> = HashMap::new();
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &HashMap::new(),
    };
    let result = vf
        .evaluate("conda", "env", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!(""),
        "conda.env must return empty string when CONDA_DEFAULT_ENV is unset; got: {result}"
    );
}
