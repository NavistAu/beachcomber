use beachcomber::config::ScriptProviderConfig;
use beachcomber::provider::InvalidationStrategy;
use beachcomber::provider::Provider;
use beachcomber::provider::SourceScope;
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
    assert_eq!(meta.sources.len(), 1);
    // Default scope is global.
    assert_eq!(meta.sources[0].scope, SourceScope::Global);
}

#[test]
fn script_provider_executes_json_output() {
    let config = ScriptProviderConfig {
        command: r#"echo '{"key":"value","num":42}'"#.to_string(),
        ..Default::default()
    };
    let p = ScriptProvider::new("json_test", config);
    let sources = p.sources();
    let result = sources[0].execute(None);
    assert!(!result.fields.is_empty(), "Should parse JSON output");
    assert_eq!(result.fields.get("key").unwrap().as_text(), "value");
    assert_eq!(result.fields.get("num").unwrap().as_text(), "42");
}

#[test]
fn script_provider_executes_kv_output() {
    let config = ScriptProviderConfig {
        command: "printf 'name=test\\ncount=5\\n'".to_string(),
        output: Some("kv".to_string()),
        ..Default::default()
    };
    let p = ScriptProvider::new("kv_test", config);
    let sources = p.sources();
    let result = sources[0].execute(None);
    assert!(!result.fields.is_empty(), "Should parse kv output");
    assert_eq!(result.fields.get("name").unwrap().as_text(), "test");
    assert_eq!(result.fields.get("count").unwrap().as_text(), "5");
}

#[test]
fn script_provider_path_scoped_metadata() {
    // When scope = "path", the source metadata should declare PathScoped.
    let config = ScriptProviderConfig {
        command: r#"echo '{"cwd":"test"}'"#.to_string(),
        scope: Some("path".to_string()),
        ..Default::default()
    };
    let p = ScriptProvider::new("path_test", config);
    let meta = p.metadata();
    assert_eq!(meta.sources[0].scope, SourceScope::PathScoped);
}

#[test]
fn script_provider_path_scoped_executes_with_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().to_str().unwrap().to_string();
    let config = ScriptProviderConfig {
        command: r#"echo '{"cwd":"test"}'"#.to_string(),
        scope: Some("path".to_string()),
        ..Default::default()
    };
    let p = ScriptProvider::new("path_test", config);
    let sources = p.sources();
    let result = sources[0].execute(Some(&path));
    assert!(!result.fields.is_empty(), "path-scoped execute should return fields");
}

#[test]
fn script_provider_returns_empty_on_failure() {
    let config = ScriptProviderConfig {
        command: "false".to_string(),
        ..Default::default()
    };
    let p = ScriptProvider::new("fail_test", config);
    let sources = p.sources();
    let result = sources[0].execute(None);
    assert!(result.fields.is_empty(), "Failed command should return empty SourceResult");
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
    match &meta.sources[0].invalidation {
        InvalidationStrategy::Poll { interval_secs } => {
            assert_eq!(*interval_secs, 10);
        }
        _ => panic!("Expected Poll invalidation"),
    }
}

#[test]
fn script_provider_with_watch_patterns() {
    let config = ScriptProviderConfig {
        command: "echo '{}'".to_string(),
        scope: Some("path".to_string()), // PathScoped required for WatchAndPoll
        invalidation: Some(beachcomber::config::ScriptInvalidation {
            poll: Some("60s".to_string()),
            watch: Some(vec!["Cargo.toml".to_string(), "Cargo.lock".to_string()]),
        }),
        ..Default::default()
    };
    let p = ScriptProvider::new("watch_test", config);
    let meta = p.metadata();
    match &meta.sources[0].invalidation {
        InvalidationStrategy::WatchAndPoll {
            patterns,
            interval_secs,
            ..
        } => {
            assert!(patterns.contains(&"Cargo.toml".to_string()));
            assert!(patterns.contains(&"Cargo.lock".to_string()));
            assert_eq!(*interval_secs, 60);
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
