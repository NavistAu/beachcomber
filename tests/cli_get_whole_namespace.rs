//! Tests for evaluate_namespace (Step 1.4).

use beachcomber::cli::virtual_fields::{VirtualFields, evaluate_namespace};
use serde_json::json;
use std::collections::HashMap;

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
