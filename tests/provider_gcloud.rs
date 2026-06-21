use beachcomber::provider::Provider;
use beachcomber::provider::Value;
use beachcomber::provider::gcloud::GcloudProvider;
use tempfile::TempDir;

// Helper: write active_config file and one config_<name>/properties under a tempdir.
fn write_config(dir: &TempDir, name: &str, project: &str, account: &str) {
    let conf_dir = dir
        .path()
        .join("configurations")
        .join(format!("config_{name}"));
    std::fs::create_dir_all(&conf_dir).unwrap();
    let props = format!("[core]\nproject = {project}\naccount = {account}\n");
    std::fs::write(conf_dir.join("properties"), &props).unwrap();
}

fn write_active_config(dir: &TempDir, name: &str) {
    std::fs::write(dir.path().join("active_config"), name).unwrap();
}

// ── metadata name ─────────────────────────────────────────────────────────────

#[test]
fn gcloud_provider_name_is_gcloud_configs() {
    assert_eq!(GcloudProvider.metadata().name, "gcloud_configs");
}

// ── data provider shape: one Object field per config + active_config String ──

#[test]
fn gcloud_configs_two_configs_and_active_config_field() {
    let dir = TempDir::new().unwrap();
    write_active_config(&dir, "default");
    write_config(&dir, "default", "proj-a", "a@x.com");
    write_config(&dir, "work", "proj-b", "b@x.com");

    let result = temp_env::with_var(
        "CLOUDSDK_CONFIG",
        Some(dir.path().to_str().unwrap()),
        || {
            let sources = GcloudProvider.sources();
            let src = sources
                .iter()
                .find(|s| s.metadata().name == "config_dir")
                .expect("config_dir source must exist");
            src.execute(None)
        },
    );

    // active_config String field
    assert_eq!(
        result.fields.get("active_config").map(|v| {
            if let Value::String(s) = v {
                s.as_str()
            } else {
                ""
            }
        }),
        Some("default"),
        "active_config must be 'default'"
    );

    // 'default' config Object{project, account}
    let default_val = result
        .fields
        .get("default")
        .expect("'default' field must be present");
    match default_val {
        Value::Object(map) => {
            assert_eq!(
                map.get("project").and_then(|v| {
                    if let Value::String(s) = v {
                        Some(s.as_str())
                    } else {
                        None
                    }
                }),
                Some("proj-a"),
                "default.project must be proj-a"
            );
            assert_eq!(
                map.get("account").and_then(|v| {
                    if let Value::String(s) = v {
                        Some(s.as_str())
                    } else {
                        None
                    }
                }),
                Some("a@x.com"),
                "default.account must be a@x.com"
            );
        }
        other => panic!("'default' must be Value::Object, got {other:?}"),
    }

    // 'work' config Object{project, account}
    let work_val = result
        .fields
        .get("work")
        .expect("'work' field must be present");
    match work_val {
        Value::Object(map) => {
            assert_eq!(
                map.get("project").and_then(|v| {
                    if let Value::String(s) = v {
                        Some(s.as_str())
                    } else {
                        None
                    }
                }),
                Some("proj-b"),
                "work.project must be proj-b"
            );
            assert_eq!(
                map.get("account").and_then(|v| {
                    if let Value::String(s) = v {
                        Some(s.as_str())
                    } else {
                        None
                    }
                }),
                Some("b@x.com"),
                "work.account must be b@x.com"
            );
        }
        other => panic!("'work' must be Value::Object, got {other:?}"),
    }

    // No 'configs' wrapper, no 'config_project' field
    assert!(
        !result.fields.contains_key("configs"),
        "must not have a 'configs' wrapper key"
    );
    assert!(
        !result.fields.contains_key("config_project"),
        "must not have the old 'config_project' field"
    );
}

// ── $CLOUDSDK_ACTIVE_CONFIG_NAME does NOT change active_config ────────────────

#[test]
fn gcloud_active_config_name_env_does_not_override_file() {
    let dir = TempDir::new().unwrap();
    write_active_config(&dir, "default");
    write_config(&dir, "default", "proj-a", "a@x.com");

    let result = temp_env::with_vars(
        [
            ("CLOUDSDK_CONFIG", Some(dir.path().to_str().unwrap())),
            ("CLOUDSDK_ACTIVE_CONFIG_NAME", Some("work")),
        ],
        || {
            let sources = GcloudProvider.sources();
            let src = sources
                .iter()
                .find(|s| s.metadata().name == "config_dir")
                .expect("config_dir source must exist");
            src.execute(None)
        },
    );

    // active_config must still be "default" from the file, not "work" from env
    assert_eq!(
        result.fields.get("active_config").map(|v| {
            if let Value::String(s) = v {
                s.as_str()
            } else {
                ""
            }
        }),
        Some("default"),
        "active_config must follow the active_config file, not $CLOUDSDK_ACTIVE_CONFIG_NAME"
    );
}

// ── missing active_config file still returns configs ─────────────────────────

#[test]
fn gcloud_configs_without_active_config_file_still_enumerates() {
    let dir = TempDir::new().unwrap();
    // No active_config file
    write_config(&dir, "default", "proj-a", "a@x.com");

    let result = temp_env::with_var(
        "CLOUDSDK_CONFIG",
        Some(dir.path().to_str().unwrap()),
        || {
            let sources = GcloudProvider.sources();
            let src = sources
                .iter()
                .find(|s| s.metadata().name == "config_dir")
                .expect("config_dir source must exist");
            src.execute(None)
        },
    );

    // No active_config field (file absent), but config objects still present
    assert!(
        !result.fields.contains_key("active_config"),
        "active_config must be absent when file is missing"
    );
    assert!(
        result.fields.contains_key("default"),
        "'default' config must still be enumerated"
    );
}

// ── no configs and no active_config → empty result ───────────────────────────

#[test]
fn gcloud_configs_empty_dir_returns_empty() {
    let dir = TempDir::new().unwrap();
    // Empty: no active_config, no configurations/

    let result = temp_env::with_var(
        "CLOUDSDK_CONFIG",
        Some(dir.path().to_str().unwrap()),
        || {
            let sources = GcloudProvider.sources();
            let src = sources
                .iter()
                .find(|s| s.metadata().name == "config_dir")
                .expect("config_dir source must exist");
            src.execute(None)
        },
    );

    assert!(
        result.fields.is_empty(),
        "empty dir must produce empty result; got: {:?}",
        result.fields
    );
}

// ── configs with both fields empty are skipped ────────────────────────────────

#[test]
fn gcloud_configs_skips_config_with_empty_project_and_account() {
    let dir = TempDir::new().unwrap();
    write_active_config(&dir, "default");
    // 'empty' config: properties file but no project/account values
    let conf_dir = dir.path().join("configurations").join("config_empty");
    std::fs::create_dir_all(&conf_dir).unwrap();
    std::fs::write(conf_dir.join("properties"), "[core]\n").unwrap();
    // 'default' with real values
    write_config(&dir, "default", "proj-a", "a@x.com");

    let result = temp_env::with_var(
        "CLOUDSDK_CONFIG",
        Some(dir.path().to_str().unwrap()),
        || {
            let sources = GcloudProvider.sources();
            let src = sources
                .iter()
                .find(|s| s.metadata().name == "config_dir")
                .expect("config_dir source must exist");
            src.execute(None)
        },
    );

    assert!(
        !result.fields.contains_key("empty"),
        "'empty' config (no project/account) must be skipped"
    );
    assert!(
        result.fields.contains_key("default"),
        "'default' config must be present"
    );
}

// ── padded section header [ core ] is parsed correctly ───────────────────────

#[test]
fn gcloud_padded_core_section_header_is_parsed() {
    let dir = TempDir::new().unwrap();
    write_active_config(&dir, "default");

    // Write a properties file with interior-spaced section header
    let conf_dir = dir.path().join("configurations").join("config_default");
    std::fs::create_dir_all(&conf_dir).unwrap();
    std::fs::write(
        conf_dir.join("properties"),
        "[ core ]\nproject = padded-project\naccount = padded@x.com\n",
    )
    .unwrap();

    let result = temp_env::with_var(
        "CLOUDSDK_CONFIG",
        Some(dir.path().to_str().unwrap()),
        || {
            let sources = GcloudProvider.sources();
            let src = sources
                .iter()
                .find(|s| s.metadata().name == "config_dir")
                .expect("config_dir source must exist");
            src.execute(None)
        },
    );

    let default_val = result
        .fields
        .get("default")
        .expect("'default' config must be present even with padded [ core ] header");
    match default_val {
        Value::Object(map) => {
            assert_eq!(
                map.get("project").and_then(|v| {
                    if let Value::String(s) = v {
                        Some(s.as_str())
                    } else {
                        None
                    }
                }),
                Some("padded-project"),
                "project must be parsed from [ core ] (padded) section"
            );
            assert_eq!(
                map.get("account").and_then(|v| {
                    if let Value::String(s) = v {
                        Some(s.as_str())
                    } else {
                        None
                    }
                }),
                Some("padded@x.com"),
                "account must be parsed from [ core ] (padded) section"
            );
        }
        other => panic!("'default' must be Value::Object, got {other:?}"),
    }
}

// ── source metadata shape ─────────────────────────────────────────────────────

#[test]
fn gcloud_provider_source_has_active_config_and_dynamic_sentinel() {
    let meta = GcloudProvider.metadata();
    assert_eq!(meta.sources.len(), 1);
    let src = &meta.sources[0];
    assert_eq!(src.name, "config_dir");
    let field_names: Vec<&str> = src.fields.iter().map(|f| f.name.as_str()).collect();
    // Fixed active_config field
    assert!(
        field_names.contains(&"active_config"),
        "source must declare 'active_config' field; got: {field_names:?}"
    );
    // Dynamic config sentinel
    assert!(
        field_names.iter().any(|n| n.starts_with('<')),
        "source must declare a dynamic field sentinel; got: {field_names:?}"
    );
}
