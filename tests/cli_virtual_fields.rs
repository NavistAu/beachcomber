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
    daemon_data.insert("terraform.path_workspace".to_string(), json!("staging"));
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &daemon_data,
    };
    let result = vf
        .evaluate_expression(
            "env.TF_WORKSPACE or terraform.path_workspace",
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
    daemon_data.insert("terraform.path_workspace".to_string(), json!("staging"));
    let ctx = EvalContext {
        env_vars: &env_vars,
        daemon_data: &daemon_data,
    };
    let result = vf
        .evaluate_expression(
            "env.TF_WORKSPACE or terraform.path_workspace",
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
    use beachcomber::cli::virtual_fields::discover_expression_refs;
    let refs = discover_expression_refs("env.A or provider.field or other.field2");
    assert!(
        refs.iter().any(|(p, f)| p == "env" && f == "A"),
        "env.A missing"
    );
    assert!(
        refs.iter().any(|(p, f)| p == "provider" && f == "field"),
        "provider.field missing"
    );
    assert!(
        refs.iter().any(|(p, f)| p == "other" && f == "field2"),
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
    // Before fix: expression was "env.CLOUDSDK_CORE_PROJECT or gcloud.project"
    // where gcloud.project IS this virtual field → cycle detected → error.
    // After fix: expression falls back to gcloud.config_project (daemon intrinsic).
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

    // Case 2: env unset + daemon has gcloud.config_project → must return daemon value.
    {
        let env_vars: HashMap<String, String> = HashMap::new();
        let mut daemon_data: HashMap<String, serde_json::Value> = HashMap::new();
        daemon_data.insert("gcloud.config_project".to_string(), json!("fromfile"));
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
    // foo.object.key must yield ("foo", "object") — deeper segments are MiniJinja
    // attribute navigation into the fetched value, not part of the daemon key.
    use beachcomber::cli::virtual_fields::discover_expression_refs;
    let refs = discover_expression_refs("foo.object.key");
    assert_eq!(refs.len(), 1, "one ref expected; got: {refs:?}");
    assert!(
        refs.iter().any(|(p, f)| p == "foo" && f == "object"),
        "expected (\"foo\", \"object\"); got: {refs:?}"
    );
}

#[test]
fn nested_ref_discovery_four_segments_and_two_segment_ref() {
    // "a.b.c.d or env.X" → [("a", "b"), ("env", "X")]
    use beachcomber::cli::virtual_fields::discover_expression_refs;
    let refs = discover_expression_refs("a.b.c.d or env.X");
    assert_eq!(refs.len(), 2, "two refs expected; got: {refs:?}");
    assert!(
        refs.iter().any(|(p, f)| p == "a" && f == "b"),
        "expected (\"a\", \"b\"); got: {refs:?}"
    );
    assert!(
        refs.iter().any(|(p, f)| p == "env" && f == "X"),
        "expected (\"env\", \"X\"); got: {refs:?}"
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
