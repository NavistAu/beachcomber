use beachcomber::provider::Provider;
use beachcomber::provider::aws::AwsProvider;
use tempfile::TempDir;

fn write_aws_config(dir: &TempDir, content: &str) -> std::path::PathBuf {
    let config_path = dir.path().join("config");
    std::fs::write(&config_path, content).unwrap();
    config_path
}

#[test]
fn aws_config_region_reads_default_profile() {
    let dir = TempDir::new().unwrap();
    write_aws_config(&dir, "[default]\nregion = eu-west-1\noutput = json\n");

    // The aws provider needs a way to know the config path. We use the
    // AWS_CONFIG_FILE env var (standard AWS SDK convention) to override the
    // default ~/.aws/config in tests.
    let result = temp_env::with_var(
        "AWS_CONFIG_FILE",
        Some(dir.path().join("config").to_str().unwrap()),
        || {
            let sources = AwsProvider.sources();
            // Find the config_file source (not profile).
            let cfg_src = sources
                .iter()
                .find(|s| s.metadata().name == "config_file")
                .expect("config_file source must exist");
            cfg_src.execute(None)
        },
    );
    assert_eq!(
        result.fields.get("config_region").unwrap().as_text(),
        "eu-west-1",
        "config_region must be the default-profile region from ~/.aws/config"
    );
}

#[test]
fn aws_config_region_missing_default_profile_returns_empty() {
    let dir = TempDir::new().unwrap();
    write_aws_config(&dir, "[profile staging]\nregion = us-east-1\n");

    let result = temp_env::with_var(
        "AWS_CONFIG_FILE",
        Some(dir.path().join("config").to_str().unwrap()),
        || {
            let sources = AwsProvider.sources();
            let cfg_src = sources
                .iter()
                .find(|s| s.metadata().name == "config_file")
                .unwrap();
            cfg_src.execute(None)
        },
    );
    // No [default] section → empty result or absent field.
    assert!(
        result
            .fields
            .get("config_region")
            .map(|v| v.as_text() == "")
            .unwrap_or(true),
        "no default profile → config_region must be empty or absent"
    );
}

#[test]
fn aws_profile_source_no_longer_reads_env() {
    // Confirms the profile source doesn't read AWS_PROFILE / AWS_VAULT / AWS_REGION.
    // Those are now client-side virtual fields (expression form).
    assert!(
        AwsProvider
            .sources()
            .iter()
            .all(|s| s.metadata().name != "profile"),
        "profile source must be gone"
    );
    temp_env::with_vars(
        [
            ("AWS_PROFILE", Some("test-profile")),
            ("AWS_REGION", Some("us-west-2")),
        ],
        || {
            let sources = AwsProvider.sources();
            // The profile source (if it still exists as a thin shell) should return empty
            // now that env reads are removed. If the source is gone, that's also fine —
            // the registry will have one source: config_file.
            let profile_src = sources.iter().find(|s| s.metadata().name == "profile");
            match profile_src {
                None => {
                    // Profile source was removed entirely — correct, env reads gone.
                }
                Some(src) => {
                    let result = src.execute(None);
                    // If the source still exists, it must not return env-derived fields.
                    assert!(
                        !result.fields.contains_key("profile"),
                        "profile source must not read $AWS_PROFILE from env"
                    );
                }
            }
        },
    );
}

#[test]
fn aws_source_field_named_config_region_not_region() {
    // Confirms the daemon intrinsic is 'config_region', not 'region'.
    // 'region' is the virtual cascade: "env.AWS_REGION or ... or aws.config_region"
    let meta = AwsProvider.metadata();
    let all_fields: Vec<&str> = meta
        .sources
        .iter()
        .flat_map(|s| s.fields.iter().map(|f| f.name.as_str()))
        .collect();
    assert!(
        all_fields.contains(&"config_region"),
        "daemon must expose 'config_region'; fields: {all_fields:?}"
    );
    assert!(
        !all_fields.contains(&"region"),
        "'region' must not be a daemon field; it is a virtual cascade (expression form); fields: {all_fields:?}"
    );
}
