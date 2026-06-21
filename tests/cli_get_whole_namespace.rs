//! Tests for evaluate_namespace (Step 1.4).

use beachcomber::cli::virtual_fields::{VirtualFields, evaluate_namespace};
use serde_json::json;
use std::collections::HashMap;

/// Mirrors the merge logic in `run_get`'s bare-virtual-namespace path:
/// daemon fields go in first; virtual fields overwrite on key collision.
/// This is the same code path exercised by `comb get python`.
fn merge_daemon_and_virtual(
    provider: &str,
    vf: &VirtualFields,
    env: &HashMap<String, String>,
    daemon_data: &HashMap<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let ns_result = evaluate_namespace(provider, vf, env, daemon_data);
    let mut merged = serde_json::Map::new();
    // Daemon whole-provider object goes in first (fetch_daemon_deps_for_namespace stores
    // it as daemon_data[provider] — C-1 fix ensures this is always present).
    if let Some(serde_json::Value::Object(daemon_map)) = daemon_data.get(provider) {
        for (k, v) in daemon_map {
            merged.insert(k.clone(), v.clone());
        }
    }
    // Virtual fields overwrite on collision.
    if let serde_json::Value::Object(ns_map) = ns_result {
        for (k, v) in ns_map {
            merged.insert(k, v);
        }
    }
    merged
}

/// C-1 regression: a whole-provider query for a provider that has BOTH daemon-cached
/// fields AND field-form-ref virtual fields must return ALL fields — not just the
/// virtual fields.
///
/// Scenario: `python` provider has daemon-cached fields (`venv`, `local_venv_name`,
/// `venv_version`) and two virtual fields (`version`, `venv_name`) whose expressions
/// reference individual fields via `python.venv_version` / `python.local_venv_name`
/// (Resolved refs, not CacheProvider).
///
/// Before the fix, `daemon_data["python"]` was never populated when virtual
/// expressions used only field-form refs (no `cache.python`), so the merge at
/// `daemon_data.get("python")` found nothing and the daemon fields were silently
/// dropped.
///
/// After the fix, `fetch_daemon_deps_for_namespace` unconditionally fetches the
/// whole provider object and stores it as `daemon_data["python"]`, so the merge
/// includes daemon fields alongside virtual fields.
#[test]
fn whole_provider_merge_includes_daemon_fields_when_virtual_uses_field_refs() {
    let vf = VirtualFields::defaults_only();
    let env: HashMap<String, String> = HashMap::new();

    // Simulate what fetch_daemon_deps_for_namespace now produces after the C-1 fix:
    // - daemon_data["python"] = whole provider object (always fetched)
    // - daemon_data["python.venv_version"] = individual field (from Resolved ref in `version`)
    // - daemon_data["python.local_venv_name"] = individual field (from Resolved ref in `venv_name`)
    let daemon_data: HashMap<String, serde_json::Value> = [
        (
            "python".to_string(),
            json!({
                "venv": "/home/user/.venvs/myproject",
                "local_venv_name": "myproject",
                "venv_version": "3.11.2"
            }),
        ),
        ("python.venv_version".to_string(), json!("3.11.2")),
        ("python.local_venv_name".to_string(), json!("myproject")),
    ]
    .into_iter()
    .collect();

    let merged = merge_daemon_and_virtual("python", &vf, &env, &daemon_data);

    // Daemon-cached fields must be present (C-1 fix).
    assert_eq!(
        merged.get("venv").cloned(),
        Some(json!("/home/user/.venvs/myproject")),
        "daemon field 'venv' must survive the merge"
    );
    assert_eq!(
        merged.get("local_venv_name").cloned(),
        Some(json!("myproject")),
        "daemon field 'local_venv_name' must be present"
    );
    assert_eq!(
        merged.get("venv_version").cloned(),
        Some(json!("3.11.2")),
        "daemon field 'venv_version' must be present"
    );

    // Virtual fields must also be present (virtual wins on collision).
    // python.version cascades: no env vars set → falls through to python.venv_version.
    assert_eq!(
        merged.get("version").cloned(),
        Some(json!("3.11.2")),
        "virtual field 'version' must be present (resolved from python.venv_version)"
    );
    // python.venv_name cascades: python.local_venv_name = "myproject".
    assert_eq!(
        merged.get("venv_name").cloned(),
        Some(json!("myproject")),
        "virtual field 'venv_name' must be present (resolved from python.local_venv_name)"
    );
}

#[test]
fn evaluate_namespace_terraform_with_env() {
    let vf = VirtualFields::defaults_only();
    let env: HashMap<String, String> = [("TF_WORKSPACE".to_string(), "prod".to_string())]
        .into_iter()
        .collect();
    let daemon: HashMap<String, serde_json::Value> = HashMap::new();

    let result = evaluate_namespace("terraform", &vf, &env, &daemon);
    let obj = result.as_object().expect("must return object");
    assert_eq!(obj["workspace"], json!("prod"));
}

#[test]
fn evaluate_namespace_empty_provider_returns_empty_object() {
    let vf = VirtualFields::defaults_only();
    let env: HashMap<String, String> = HashMap::new();
    let daemon: HashMap<String, serde_json::Value> = HashMap::new();

    // "git" has no virtual fields — should return an empty object
    let result = evaluate_namespace("git", &vf, &env, &daemon);
    let obj = result
        .as_object()
        .expect("must return object even when empty");
    assert!(
        obj.is_empty(),
        "git namespace has no virtual fields; object must be empty"
    );
}

#[test]
fn evaluate_namespace_aws_uses_daemon_data() {
    let vf = VirtualFields::defaults_only();
    let env: HashMap<String, String> = [("AWS_REGION".to_string(), "ap-southeast-2".to_string())]
        .into_iter()
        .collect();
    let daemon: HashMap<String, serde_json::Value> = HashMap::new();

    let result = evaluate_namespace("aws", &vf, &env, &daemon);
    let obj = result.as_object().expect("must return object");
    assert_eq!(
        obj["region"],
        json!("ap-southeast-2"),
        "AWS_REGION must win in aws.region"
    );
}
