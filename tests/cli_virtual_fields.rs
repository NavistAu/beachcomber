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
