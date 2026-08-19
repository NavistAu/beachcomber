//! TDD tests for Task 1: kinded Ref enum and cache.* evaluator.
//!
//! Write-first (failing), then implement in src/cli/virtual_fields.rs.

use libbeachcomber::virtual_fields::{EvalContext, Ref, VirtualFields, discover_expression_refs};
use serde_json::json;
use std::collections::{HashMap, HashSet};

// ── Step 1.1: Ref discovery returns kinded refs ───────────────────────────────

#[test]
fn discovers_env_cache_and_resolved_refs() {
    let refs = discover_expression_refs(
        r#"env.AWS_REGION or cache.aws_profiles[env.AWS_PROFILE or "default"].region or aws.profile"#,
    );
    assert!(
        refs.contains(&Ref::Env("AWS_REGION".into())),
        "must discover Env(AWS_REGION); got: {refs:?}"
    );
    assert!(
        refs.contains(&Ref::Env("AWS_PROFILE".into())),
        "must discover Env(AWS_PROFILE); got: {refs:?}"
    );
    assert!(
        refs.contains(&Ref::CacheProvider("aws_profiles".into())),
        "must discover CacheProvider(aws_profiles); got: {refs:?}"
    );
    assert!(
        refs.contains(&Ref::Resolved("aws".into(), "profile".into())),
        "must discover Resolved(aws, profile); got: {refs:?}"
    );
}

#[test]
fn discovers_cache_field_ref() {
    let refs = discover_expression_refs("cache.mise.python or cache.python.venv_version");
    assert!(
        refs.contains(&Ref::CacheField("mise".into(), "python".into())),
        "must discover CacheField(mise, python); got: {refs:?}"
    );
    assert!(
        refs.contains(&Ref::CacheField("python".into(), "venv_version".into())),
        "must discover CacheField(python, venv_version); got: {refs:?}"
    );
}

#[test]
fn discovers_cache_provider_without_field() {
    // "cache.aws_profiles" (two segments only) → CacheProvider
    let refs = discover_expression_refs("cache.aws_profiles");
    assert!(
        refs.contains(&Ref::CacheProvider("aws_profiles".into())),
        "must discover CacheProvider(aws_profiles); got: {refs:?}"
    );
    // Must NOT produce a CacheField entry for it
    let has_field = refs
        .iter()
        .any(|r| matches!(r, Ref::CacheField(p, _) if p == "aws_profiles"));
    assert!(
        !has_field,
        "must not produce CacheField for two-segment cache ref; got: {refs:?}"
    );
}

#[test]
fn bare_name_is_ignored() {
    // A bare name with no dot is not a provider.field ref — must be ignored.
    let refs = discover_expression_refs("somename");
    assert!(refs.is_empty(), "bare name must be ignored; got: {refs:?}");
}

#[test]
fn cwd_is_ignored() {
    // cwd is a path-expression variable, not a field ref — must be ignored.
    let refs = discover_expression_refs("cwd or env.HOME");
    assert!(
        !refs
            .iter()
            .any(|r| matches!(r, Ref::Resolved(p, _) if p == "cwd")),
        "cwd must be ignored; got: {refs:?}"
    );
    // env.HOME must still be discovered
    assert!(
        refs.contains(&Ref::Env("HOME".into())),
        "env.HOME must still be discovered; got: {refs:?}"
    );
}

// ── Step 1.2: Context assembly + evaluation ───────────────────────────────────

#[test]
fn cache_field_reads_raw_value() {
    let vf = VirtualFields::defaults_only();
    let env: HashMap<String, String> = HashMap::new();
    let daemon: HashMap<String, serde_json::Value> =
        [("terraform.workspace".to_string(), json!("staging"))]
            .into_iter()
            .collect();
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &daemon,
    };
    let got = vf
        .evaluate_expression(
            "env.TF_WORKSPACE or cache.terraform.workspace",
            &ctx,
            &mut HashSet::new(),
        )
        .unwrap();
    assert_eq!(got, json!("staging"));
}

#[test]
fn cache_field_env_wins_over_raw_cache() {
    let vf = VirtualFields::defaults_only();
    let env: HashMap<String, String> = [("TF_WORKSPACE".to_string(), "dev".to_string())]
        .into_iter()
        .collect();
    let daemon: HashMap<String, serde_json::Value> =
        [("terraform.workspace".to_string(), json!("staging"))]
            .into_iter()
            .collect();
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &daemon,
    };
    let got = vf
        .evaluate_expression(
            "env.TF_WORKSPACE or cache.terraform.workspace",
            &ctx,
            &mut HashSet::new(),
        )
        .unwrap();
    assert_eq!(got, json!("dev"), "env.TF_WORKSPACE must win over cache");
}

#[test]
fn cache_provider_object_is_indexable() {
    let vf = VirtualFields::defaults_only();
    let env: HashMap<String, String> = [("AWS_PROFILE".to_string(), "staging".to_string())]
        .into_iter()
        .collect();
    let daemon: HashMap<String, serde_json::Value> = [(
        "aws_profiles".to_string(),
        json!({"default":{"region":"eu-west-1"},"staging":{"region":"us-east-1"}}),
    )]
    .into_iter()
    .collect();
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &daemon,
    };
    let got = vf
        .evaluate_expression(
            r#"cache.aws_profiles[env.AWS_PROFILE or "default"].region"#,
            &ctx,
            &mut HashSet::new(),
        )
        .unwrap();
    assert_eq!(got, json!("us-east-1"));
}

#[test]
fn cache_provider_object_defaults_when_selector_unset() {
    let vf = VirtualFields::defaults_only();
    let env: HashMap<String, String> = HashMap::new(); // AWS_PROFILE unset
    let daemon: HashMap<String, serde_json::Value> = [(
        "aws_profiles".to_string(),
        json!({"default":{"region":"eu-west-1"},"staging":{"region":"us-east-1"}}),
    )]
    .into_iter()
    .collect();
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &daemon,
    };
    let got = vf
        .evaluate_expression(
            r#"cache.aws_profiles[env.AWS_PROFILE or "default"].region"#,
            &ctx,
            &mut HashSet::new(),
        )
        .unwrap();
    assert_eq!(
        got,
        json!("eu-west-1"),
        "unset selector must fall back to 'default'"
    );
}

// ── Step 1.3: Resolved ref — daemon field vs virtual recursion ────────────────

#[test]
fn resolved_ref_reads_daemon_value_for_non_virtual() {
    // "git.branch" is not virtual — Resolved(git, branch) → daemon_data["git.branch"]
    let vf = VirtualFields::defaults_only();
    let env: HashMap<String, String> = HashMap::new();
    let daemon: HashMap<String, serde_json::Value> = [("git.branch".to_string(), json!("main"))]
        .into_iter()
        .collect();
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &daemon,
    };
    let got = vf
        .evaluate_expression("git.branch", &ctx, &mut HashSet::new())
        .unwrap();
    assert_eq!(got, json!("main"));
}

#[test]
fn resolved_ref_recurses_for_virtual_field() {
    // "terraform.workspace" IS virtual — Resolved(terraform, workspace) must recurse.
    // Expression is: env.TF_WORKSPACE or cache.terraform.workspace
    // With TF_WORKSPACE="myws", it should return "myws".
    let vf = VirtualFields::defaults_only();
    let env: HashMap<String, String> = [("TF_WORKSPACE".to_string(), "myws".to_string())]
        .into_iter()
        .collect();
    let daemon: HashMap<String, serde_json::Value> = HashMap::new();
    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &daemon,
    };
    // We evaluate an expression that references terraform.workspace (resolved, not cache.*)
    // terraform.workspace is virtual, so it should recurse into its expression.
    let got = vf
        .evaluate_expression("terraform.workspace", &ctx, &mut HashSet::new())
        .unwrap();
    assert_eq!(
        got,
        json!("myws"),
        "resolved virtual must recurse to evaluate its expression"
    );
}

// ── Step 1.4: fields_for and evaluate_namespace ───────────────────────────────

#[test]
fn fields_for_returns_known_virtual_fields() {
    let vf = VirtualFields::defaults_only();
    let fields = vf.fields_for("terraform");
    assert!(
        fields.contains(&"workspace".to_string()),
        "terraform.workspace must be listed"
    );
}

#[test]
fn fields_for_returns_empty_for_daemon_only_provider() {
    let vf = VirtualFields::defaults_only();
    // "git" has no virtual fields in the built-ins
    let fields = vf.fields_for("git");
    assert!(fields.is_empty(), "git has no virtual fields");
}

#[test]
fn evaluate_namespace_returns_all_virtual_fields_as_object() {
    use libbeachcomber::virtual_fields::evaluate_namespace;

    let vf = VirtualFields::defaults_only();
    let env: HashMap<String, String> = [("TF_WORKSPACE".to_string(), "ns-test".to_string())]
        .into_iter()
        .collect();
    let daemon: HashMap<String, serde_json::Value> = HashMap::new();

    let result = evaluate_namespace("terraform", &vf, &env, &daemon);
    let obj = result
        .as_object()
        .expect("evaluate_namespace must return an object");
    assert!(
        obj.contains_key("workspace"),
        "object must contain 'workspace'"
    );
    assert_eq!(obj["workspace"], json!("ns-test"));
}

#[test]
fn evaluate_namespace_all_virtual_fields_present() {
    use libbeachcomber::virtual_fields::evaluate_namespace;

    let vf = VirtualFields::defaults_only();
    let env: HashMap<String, String> = HashMap::new();
    // aws.region uses cache.aws_profiles[...].region — provide daemon data so it evaluates.
    let daemon: HashMap<String, serde_json::Value> = [(
        "aws_profiles".to_string(),
        serde_json::json!({"default":{"region":"us-east-1"}}),
    )]
    .into_iter()
    .collect();

    // "aws" namespace has profile, region, expiration virtual fields
    let result = evaluate_namespace("aws", &vf, &env, &daemon);
    let obj = result
        .as_object()
        .expect("evaluate_namespace must return an object");
    assert!(
        obj.contains_key("profile"),
        "aws object must contain 'profile'"
    );
    assert!(
        obj.contains_key("region"),
        "aws object must contain 'region'"
    );
    assert!(
        obj.contains_key("expiration"),
        "aws object must contain 'expiration'"
    );
}

// ── Fix 1 regression: CacheProvider + CacheField coexist for same provider ───

/// The gcloud.project expression uses BOTH `cache.gcloud_configs` (CacheProvider)
/// and `cache.gcloud_configs.active_config` (CacheField) in a single expression.
/// This test verifies that binding both refs for the SAME provider in one
/// expression does not lose either: the whole-object keys survive and the
/// separately-provided CacheField value is also present.
///
/// daemon_data provides:
///   "gcloud_configs" (CacheProvider key) = whole object WITHOUT active_config embedded
///   "gcloud_configs.active_config" (CacheField key) = "default"
///
/// The expression must resolve gcloud_configs.active_config → "default" and
/// gcloud_configs["default"].project → "proj-default".
#[test]
fn cache_provider_and_cache_field_same_provider_both_visible() {
    let vf = VirtualFields::defaults_only();
    let env: HashMap<String, String> = HashMap::new();

    // Whole object WITHOUT active_config key embedded — only the config entries.
    let whole_without_active = json!({
        "default": {"project": "proj-default", "account": "default@example.com"},
        "work":    {"project": "proj-work",    "account": "work@example.com"}
    });
    // active_config provided as a separate CacheField entry (key "gcloud_configs.active_config").
    let daemon: HashMap<String, serde_json::Value> = [
        ("gcloud_configs".to_string(), whole_without_active),
        ("gcloud_configs.active_config".to_string(), json!("default")),
    ]
    .into_iter()
    .collect();

    let ctx = EvalContext {
        env_vars: &env,
        daemon_data: &daemon,
    };

    // Evaluate gcloud.project:
    //   env.CLOUDSDK_CORE_PROJECT (unset)
    //   or cache.gcloud_configs[ env.CLOUDSDK_ACTIVE_CONFIG_NAME or cache.gcloud_configs.active_config ].project
    // → cache.gcloud_configs.active_config = "default"
    // → cache.gcloud_configs["default"].project = "proj-default"
    let result = vf
        .evaluate("gcloud", "project", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        result,
        json!("proj-default"),
        "CacheProvider whole-object and CacheField active_config for same provider must both be visible in one expression; got: {result}"
    );

    // Also verify gcloud.account resolves correctly in the same setup.
    let acct = vf
        .evaluate("gcloud", "account", &ctx, &mut Default::default())
        .unwrap();
    assert_eq!(
        acct,
        json!("default@example.com"),
        "gcloud.account must also resolve via the merged provider context; got: {acct}"
    );
}
