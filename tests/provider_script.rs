use beachcomber::config::ScriptProviderConfig;
use beachcomber::provider::FieldScope;
use beachcomber::provider::Provider;
use beachcomber::provider::script::ScriptProvider;

#[test]
fn script_provider_metadata() {
    let config = ScriptProviderConfig {
        command: "echo hello".to_string(),
        ..Default::default()
    };
    let p = ScriptProvider::new("test_script", config);
    let meta = p.metadata();
    assert_eq!(meta.name, "test_script");
    assert_eq!(
        meta.inferred_scope(),
        FieldScope::Global,
        "Default scope is global"
    );
}

#[test]
fn script_provider_executes_json_output() {
    let config = ScriptProviderConfig {
        command: r#"echo '{"key":"value","num":42}'"#.to_string(),
        ..Default::default()
    };
    let p = ScriptProvider::new("json_test", config);
    let (_, result) = p
        .execute(None)
        .into_iter()
        .next()
        .expect("Should parse JSON output");
    assert_eq!(result.get("key").unwrap().as_text(), "value");
    assert_eq!(result.get("num").unwrap().as_text(), "42");
}

#[test]
fn script_provider_executes_kv_output() {
    let config = ScriptProviderConfig {
        command: "printf 'name=test\\ncount=5\\n'".to_string(),
        output: Some("kv".to_string()),
        ..Default::default()
    };
    let p = ScriptProvider::new("kv_test", config);
    let (_, result) = p
        .execute(None)
        .into_iter()
        .next()
        .expect("Should parse kv output");
    assert_eq!(result.get("name").unwrap().as_text(), "test");
    assert_eq!(result.get("count").unwrap().as_text(), "5");
}

#[test]
fn script_provider_path_scoped() {
    // NOTE: FieldSchema.scope is a placeholder (Global) until Task 11 wires
    // config.scope into per-field scope. The execute() path still correctly
    // produces path-scoped cache entries when scope = "path" is configured.
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().to_str().unwrap().to_string();
    let config = ScriptProviderConfig {
        command: r#"echo '{"cwd":"test"}'"#.to_string(),
        scope: Some("path".to_string()),
        ..Default::default()
    };
    let p = ScriptProvider::new("path_test", config);
    // Verify execute returns a path-keyed entry, not a global (None-keyed) entry.
    let results = p.execute(Some(&path));
    assert_eq!(results.len(), 1, "should produce one result");
    let (key, _) = &results[0];
    assert_eq!(
        key.as_deref(),
        Some(path.as_str()),
        "path scope should produce a path-keyed entry"
    );
}

#[test]
fn script_provider_returns_none_on_failure() {
    let config = ScriptProviderConfig {
        command: "false".to_string(),
        ..Default::default()
    };
    let p = ScriptProvider::new("fail_test", config);
    assert!(
        p.execute(None).is_empty(),
        "Failed command should return empty Vec"
    );
}

#[test]
fn script_provider_custom_poll() {
    let config = ScriptProviderConfig {
        command: "echo '{}'".to_string(),
        invalidation: Some(beachcomber::config::ScriptInvalidation {
            poll: Some("10s".to_string()),
            watch: None,
        }),
        ..Default::default()
    };
    let p = ScriptProvider::new("poll_test", config);
    let meta = p.metadata();
    match meta.invalidation {
        beachcomber::provider::InvalidationStrategy::Poll { interval_secs, .. } => {
            assert_eq!(interval_secs, 10);
        }
        _ => panic!("Expected Poll invalidation"),
    }
}

#[test]
fn script_provider_with_watch_patterns() {
    let config = ScriptProviderConfig {
        command: "echo '{}'".to_string(),
        invalidation: Some(beachcomber::config::ScriptInvalidation {
            poll: Some("60s".to_string()),
            watch: Some(vec!["Cargo.toml".to_string(), "Cargo.lock".to_string()]),
        }),
        ..Default::default()
    };
    let p = ScriptProvider::new("watch_test", config);
    let meta = p.metadata();
    match meta.invalidation {
        beachcomber::provider::InvalidationStrategy::WatchAndPoll {
            ref patterns,
            interval_secs,
            ..
        } => {
            assert!(patterns.contains(&"Cargo.toml".to_string()));
            assert!(patterns.contains(&"Cargo.lock".to_string()));
            assert_eq!(interval_secs, 60);
        }
        _ => panic!("Expected WatchAndPoll invalidation"),
    }
}

#[test]
fn script_providers_from_config() {
    let toml_str = r#"
[providers.docker_context]
type = "script"
command = "echo '{\"context\":\"default\"}'"
invalidation = { poll = "30s" }
"#;
    let config: beachcomber::config::Config = toml::from_str(toml_str).unwrap();
    let scripts = config.script_providers();
    assert_eq!(scripts.len(), 1);
    assert_eq!(scripts[0].0, "docker_context");
}
