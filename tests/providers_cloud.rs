use beachcomber::provider::Provider;
use beachcomber::provider::SourceScope;
use beachcomber::provider::aws::AwsProvider;
use beachcomber::provider::gcloud::GcloudProvider;
use beachcomber::provider::kubecontext::KubecontextProvider;

#[test]
fn kubecontext_provider_metadata() {
    let p = KubecontextProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "kubecontext");
    assert_eq!(meta.sources.len(), 1);
    let src = &meta.sources[0];
    assert_eq!(src.name, "context");
    // PathScoped (Task 8): the source reads the kubeconfig path the CLI hands it
    // (resolved from $KUBECONFIG via the provider's path expression), not a fixed file.
    assert_eq!(src.scope, SourceScope::PathScoped);
    let fields: Vec<&str> = src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"context"));
    assert!(fields.contains(&"namespace"));
}

#[test]
fn kubecontext_executes_without_panic() {
    let p = KubecontextProvider;
    let sources = p.sources();
    let _ = sources[0].execute(None); // May return empty if kubectl not installed
}

#[test]
fn aws_provider_metadata() {
    let p = AwsProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "aws_profiles");
    assert_eq!(meta.sources.len(), 1);
    let src = &meta.sources[0];
    assert_eq!(src.name, "config_file");
    assert_eq!(src.scope, SourceScope::Global);
    // Dynamic sentinel field — profile names are not known at compile time.
    let field_names: Vec<&str> = src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(
        field_names.iter().any(|n| n.starts_with('<')),
        "source must declare a dynamic field sentinel; got: {field_names:?}"
    );
}

#[test]
fn aws_executes_without_panic() {
    let p = AwsProvider;
    let sources = p.sources();
    let _ = sources[0].execute(None);
}

#[test]
fn gcloud_provider_metadata() {
    let p = GcloudProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "gcloud_configs");
    assert_eq!(meta.sources.len(), 1);
    let src = &meta.sources[0];
    assert_eq!(src.name, "config_dir");
    assert_eq!(src.scope, SourceScope::Global);
    let field_names: Vec<&str> = src.fields.iter().map(|f| f.name.as_str()).collect();
    // Fixed active_config String field
    assert!(field_names.contains(&"active_config"));
    // Dynamic sentinel for per-config Object fields
    assert!(
        field_names.iter().any(|n| n.starts_with('<')),
        "source must declare a dynamic field sentinel; got: {field_names:?}"
    );
}

#[test]
fn gcloud_executes_without_panic() {
    let p = GcloudProvider;
    let sources = p.sources();
    let _ = sources[0].execute(None);
}

// ── gcloud seam tests ─────────────────────────────────────────────────────────
// These use CLOUDSDK_CONFIG env var to point at a tempdir with a controlled
// layout, proving the active_config indirection is followed.
// temp_env::with_var is used for all env mutations (dev-dep, Cargo.toml line 58).

#[test]
fn gcloud_reads_active_config_indirection() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("active_config"), "staging\n").unwrap();
    let cfg_dir = dir.path().join("configurations").join("config_staging");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("properties"),
        "[core]\nproject = my-project\naccount = user@example.com\n",
    )
    .unwrap();

    let result = temp_env::with_var(
        "CLOUDSDK_CONFIG",
        Some(dir.path().to_str().unwrap()),
        || {
            let sources = GcloudProvider.sources();
            sources[0].execute(None)
        },
    );

    // New shape: 'staging' is an Object with project/account sub-fields.
    // active_config is a String field with the name of the active config.
    assert_eq!(
        result.fields.get("active_config").and_then(|v| {
            if let beachcomber::provider::Value::String(s) = v {
                Some(s.as_str())
            } else {
                None
            }
        }),
        Some("staging"),
        "active_config must be 'staging'"
    );
    let staging_val = result
        .fields
        .get("staging")
        .expect("'staging' config must be present");
    match staging_val {
        beachcomber::provider::Value::Object(map) => {
            assert_eq!(
                map.get("project").and_then(|v| {
                    if let beachcomber::provider::Value::String(s) = v {
                        Some(s.as_str())
                    } else {
                        None
                    }
                }),
                Some("my-project"),
                "staging.project should be my-project"
            );
            assert_eq!(
                map.get("account").and_then(|v| {
                    if let beachcomber::provider::Value::String(s) = v {
                        Some(s.as_str())
                    } else {
                        None
                    }
                }),
                Some("user@example.com"),
                "staging.account should be user@example.com"
            );
        }
        other => panic!("'staging' must be Value::Object, got {other:?}"),
    }
}

// Note: gcloud_cloudsdk_active_config_name_overrides_active_config_file was removed in P1.
// $CLOUDSDK_ACTIVE_CONFIG_NAME is now a client-side concern (P2 live.* path).
// The daemon follows only active_config file. See tests/provider_gcloud.rs.

#[test]
fn gcloud_missing_active_config_returns_empty() {
    let dir = tempfile::tempdir().unwrap();
    // No active_config file, no configurations/
    let result = temp_env::with_var(
        "CLOUDSDK_CONFIG",
        Some(dir.path().to_str().unwrap()),
        || {
            let sources = GcloudProvider.sources();
            sources[0].execute(None)
        },
    );
    assert!(
        result.fields.is_empty(),
        "missing active_config should produce empty result"
    );
}

#[test]
fn gcloud_project_strip_not_greedy() {
    // Ensure "projectid = foo" doesn't set project = "id = foo"
    // (the old strip_prefix("project") bug).
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("active_config"), "default\n").unwrap();
    let cfg_dir = dir.path().join("configurations").join("config_default");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("properties"),
        "[core]\nprojectid = should-not-match\nproject = correct-project\n",
    )
    .unwrap();

    let result = temp_env::with_var(
        "CLOUDSDK_CONFIG",
        Some(dir.path().to_str().unwrap()),
        || {
            let sources = GcloudProvider.sources();
            sources[0].execute(None)
        },
    );

    // New shape: 'default' is an Object with a 'project' sub-field.
    let default_val = result
        .fields
        .get("default")
        .expect("'default' config must be present");
    match default_val {
        beachcomber::provider::Value::Object(map) => {
            assert_eq!(
                map.get("project").and_then(|v| {
                    if let beachcomber::provider::Value::String(s) = v {
                        Some(s.as_str())
                    } else {
                        None
                    }
                }),
                Some("correct-project"),
                "strip_prefix must not match 'projectid'; default.project must be correct-project"
            );
        }
        other => panic!("'default' must be Value::Object, got {other:?}"),
    }
}
