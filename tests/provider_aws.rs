use beachcomber::provider::Provider;
use beachcomber::provider::Value;
use beachcomber::provider::aws::AwsProvider;
use tempfile::TempDir;

fn write_aws_config(dir: &TempDir, content: &str) -> std::path::PathBuf {
    let config_path = dir.path().join("config");
    std::fs::write(&config_path, content).unwrap();
    config_path
}

// ── metadata name ─────────────────────────────────────────────────────────────

#[test]
fn aws_provider_name_is_aws_profiles() {
    assert_eq!(AwsProvider.metadata().name, "aws_profiles");
}

// ── data provider shape: fields = profile names, each an Object{region} ──────

#[test]
fn aws_profiles_default_and_staging_profiles() {
    let dir = TempDir::new().unwrap();
    write_aws_config(
        &dir,
        "[default]\nregion = eu-west-1\noutput = json\n\n[profile staging]\nregion = us-east-1\n",
    );

    let result = temp_env::with_var(
        "AWS_CONFIG_FILE",
        Some(dir.path().join("config").to_str().unwrap()),
        || {
            let sources = AwsProvider.sources();
            let src = sources
                .iter()
                .find(|s| s.metadata().name == "config_file")
                .expect("config_file source must exist");
            src.execute(None)
        },
    );

    // Both profiles present
    let default_val = result
        .fields
        .get("default")
        .expect("'default' field must be present");
    let staging_val = result
        .fields
        .get("staging")
        .expect("'staging' field must be present");

    // Each is an Object with a 'region' key (no wrapper field)
    match default_val {
        Value::Object(map) => {
            assert_eq!(
                map.get("region").and_then(|v| if let Value::String(s) = v {
                    Some(s.as_str())
                } else {
                    None
                }),
                Some("eu-west-1"),
                "default.region must be eu-west-1"
            );
        }
        other => panic!("'default' must be Value::Object, got {other:?}"),
    }
    match staging_val {
        Value::Object(map) => {
            assert_eq!(
                map.get("region").and_then(|v| if let Value::String(s) = v {
                    Some(s.as_str())
                } else {
                    None
                }),
                Some("us-east-1"),
                "staging.region must be us-east-1"
            );
        }
        other => panic!("'staging' must be Value::Object, got {other:?}"),
    }

    // No 'profiles' wrapper, no 'config_region'
    assert!(
        !result.fields.contains_key("profiles"),
        "must not have a 'profiles' wrapper key"
    );
    assert!(
        !result.fields.contains_key("config_region"),
        "must not have the old 'config_region' field"
    );
}

// ── all profiles published regardless of $AWS_PROFILE ────────────────────────

#[test]
fn aws_profiles_published_regardless_of_aws_profile_env() {
    let dir = TempDir::new().unwrap();
    write_aws_config(
        &dir,
        "[default]\nregion = eu-west-1\n\n[profile staging]\nregion = us-east-1\n",
    );

    let result = temp_env::with_vars(
        [
            (
                "AWS_CONFIG_FILE",
                Some(dir.path().join("config").to_str().unwrap()),
            ),
            ("AWS_PROFILE", Some("staging")),
        ],
        || {
            let sources = AwsProvider.sources();
            let src = sources
                .iter()
                .find(|s| s.metadata().name == "config_file")
                .expect("config_file source must exist");
            src.execute(None)
        },
    );

    assert!(
        result.fields.contains_key("default"),
        "default profile must be published even when AWS_PROFILE=staging"
    );
    assert!(
        result.fields.contains_key("staging"),
        "staging profile must be published"
    );
}

// ── missing config → empty result ─────────────────────────────────────────────

#[test]
fn aws_profiles_missing_config_returns_empty() {
    let dir = TempDir::new().unwrap();
    // No config file written

    let result = temp_env::with_var(
        "AWS_CONFIG_FILE",
        Some(dir.path().join("config").to_str().unwrap()),
        || {
            let sources = AwsProvider.sources();
            let src = sources
                .iter()
                .find(|s| s.metadata().name == "config_file")
                .expect("config_file source must exist");
            src.execute(None)
        },
    );

    assert!(
        result.fields.is_empty(),
        "missing config → empty result; got: {:?}",
        result.fields
    );
}

// ── profiles with no region are skipped ──────────────────────────────────────

#[test]
fn aws_profiles_skips_profiles_with_no_region() {
    let dir = TempDir::new().unwrap();
    write_aws_config(
        &dir,
        "[default]\noutput = json\n\n[profile staging]\nregion = us-east-1\n",
    );

    let result = temp_env::with_var(
        "AWS_CONFIG_FILE",
        Some(dir.path().join("config").to_str().unwrap()),
        || {
            let sources = AwsProvider.sources();
            let src = sources
                .iter()
                .find(|s| s.metadata().name == "config_file")
                .expect("config_file source must exist");
            src.execute(None)
        },
    );

    // 'default' has no region → skipped
    assert!(
        !result.fields.contains_key("default"),
        "default has no region; must not be published"
    );
    // 'staging' has region → published
    assert!(
        result.fields.contains_key("staging"),
        "staging has region; must be published"
    );
}

// ── source metadata shape ─────────────────────────────────────────────────────

#[test]
fn aws_provider_source_is_config_file_with_dynamic_field_sentinel() {
    let meta = AwsProvider.metadata();
    assert_eq!(meta.sources.len(), 1);
    let src = &meta.sources[0];
    assert_eq!(src.name, "config_file");
    // Dynamic sentinel (like mise's <tool>) for profile names
    let field_names: Vec<&str> = src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(
        field_names.iter().any(|n| n.starts_with('<')),
        "source must declare a dynamic field sentinel; got: {field_names:?}"
    );
}

// ── no profile source (legacy env-read source must be gone) ──────────────────

#[test]
fn aws_has_no_profile_source() {
    assert!(
        AwsProvider
            .sources()
            .iter()
            .all(|s| s.metadata().name != "profile"),
        "profile source must be gone"
    );
}
