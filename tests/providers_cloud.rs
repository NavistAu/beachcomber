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
    assert_eq!(src.scope, SourceScope::Global);
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
    assert_eq!(meta.name, "aws");
    assert_eq!(meta.sources.len(), 1);
    let src = &meta.sources[0];
    assert_eq!(src.name, "profile");
    assert_eq!(src.scope, SourceScope::Global);
    let fields: Vec<&str> = src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"profile"));
    assert!(fields.contains(&"region"));
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
    assert_eq!(meta.name, "gcloud");
    assert_eq!(meta.sources.len(), 1);
    let src = &meta.sources[0];
    assert_eq!(src.name, "config");
    assert_eq!(src.scope, SourceScope::Global);
    let fields: Vec<&str> = src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(fields.contains(&"project"));
    assert!(fields.contains(&"account"));
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

    assert_eq!(
        result.fields.get("project").unwrap().as_text(),
        "my-project",
        "project should be read from configurations/config_staging/properties"
    );
    assert_eq!(
        result.fields.get("account").unwrap().as_text(),
        "user@example.com"
    );
}

#[test]
fn gcloud_cloudsdk_active_config_name_overrides_active_config_file() {
    let dir = tempfile::tempdir().unwrap();
    // active_config file says "default" ...
    std::fs::write(dir.path().join("active_config"), "default\n").unwrap();
    // ... but env var says "prod", so we expect prod's properties.
    for name in &["default", "prod"] {
        let cfg_dir = dir
            .path()
            .join("configurations")
            .join(format!("config_{}", name));
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("properties"),
            format!(
                "[core]\nproject = {}-project\naccount = {}-user@example.com\n",
                name, name
            ),
        )
        .unwrap();
    }

    let result = temp_env::with_vars(
        [
            ("CLOUDSDK_CONFIG", Some(dir.path().to_str().unwrap())),
            ("CLOUDSDK_ACTIVE_CONFIG_NAME", Some("prod")),
        ],
        || {
            let sources = GcloudProvider.sources();
            sources[0].execute(None)
        },
    );

    assert_eq!(
        result.fields.get("project").unwrap().as_text(),
        "prod-project",
        "$CLOUDSDK_ACTIVE_CONFIG_NAME must override active_config file"
    );
}

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

    assert_eq!(
        result.fields.get("project").unwrap().as_text(),
        "correct-project",
        "strip_prefix must not match 'projectid'"
    );
}
