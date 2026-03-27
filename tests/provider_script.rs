use shellstate::provider::Provider;
use shellstate::provider::script::ScriptProvider;
use shellstate::config::ScriptProviderConfig;

#[test]
fn script_provider_metadata() {
    let config = ScriptProviderConfig {
        command: "echo hello".to_string(),
        invalidation: None,
        fields: None,
        output: None,
        scope: None,
        provider_type: None,
    };
    let p = ScriptProvider::new("test_script", config);
    let meta = p.metadata();
    assert_eq!(meta.name, "test_script");
    assert!(meta.global, "Default scope is global");
}

#[test]
fn script_provider_executes_json_output() {
    let config = ScriptProviderConfig {
        command: r#"echo '{"key":"value","num":42}'"#.to_string(),
        invalidation: None,
        fields: None,
        output: None,
        scope: None,
        provider_type: None,
    };
    let p = ScriptProvider::new("json_test", config);
    let result = p.execute(None).expect("Should parse JSON output");
    assert_eq!(result.get("key").unwrap().as_text(), "value");
    assert_eq!(result.get("num").unwrap().as_text(), "42");
}

#[test]
fn script_provider_executes_kv_output() {
    let config = ScriptProviderConfig {
        command: "printf 'name=test\\ncount=5\\n'".to_string(),
        invalidation: None,
        fields: None,
        output: Some("kv".to_string()),
        scope: None,
        provider_type: None,
    };
    let p = ScriptProvider::new("kv_test", config);
    let result = p.execute(None).expect("Should parse kv output");
    assert_eq!(result.get("name").unwrap().as_text(), "test");
    assert_eq!(result.get("count").unwrap().as_text(), "5");
}

#[test]
fn script_provider_path_scoped() {
    let config = ScriptProviderConfig {
        command: "echo '{\"cwd\":\"test\"}'".to_string(),
        invalidation: None,
        fields: None,
        output: None,
        scope: Some("path".to_string()),
        provider_type: None,
    };
    let p = ScriptProvider::new("path_test", config);
    let meta = p.metadata();
    assert!(!meta.global, "path scope should not be global");
}

#[test]
fn script_provider_returns_none_on_failure() {
    let config = ScriptProviderConfig {
        command: "false".to_string(),
        invalidation: None,
        fields: None,
        output: None,
        scope: None,
        provider_type: None,
    };
    let p = ScriptProvider::new("fail_test", config);
    assert!(p.execute(None).is_none(), "Failed command should return None");
}

#[test]
fn script_provider_custom_poll() {
    let config = ScriptProviderConfig {
        command: "echo '{}'".to_string(),
        invalidation: Some(shellstate::config::ScriptInvalidation {
            poll: Some("10s".to_string()),
            watch: None,
        }),
        fields: None,
        output: None,
        scope: None,
        provider_type: None,
    };
    let p = ScriptProvider::new("poll_test", config);
    let meta = p.metadata();
    match meta.invalidation {
        shellstate::provider::InvalidationStrategy::Poll { interval_secs, .. } => {
            assert_eq!(interval_secs, 10);
        }
        _ => panic!("Expected Poll invalidation"),
    }
}

#[test]
fn script_provider_with_watch_patterns() {
    let config = ScriptProviderConfig {
        command: "echo '{}'".to_string(),
        invalidation: Some(shellstate::config::ScriptInvalidation {
            poll: Some("60s".to_string()),
            watch: Some(vec!["Cargo.toml".to_string(), "Cargo.lock".to_string()]),
        }),
        fields: None,
        output: None,
        scope: None,
        provider_type: None,
    };
    let p = ScriptProvider::new("watch_test", config);
    let meta = p.metadata();
    match meta.invalidation {
        shellstate::provider::InvalidationStrategy::WatchAndPoll { ref patterns, interval_secs, .. } => {
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
    let config: shellstate::config::Config = toml::from_str(toml_str).unwrap();
    let scripts = config.script_providers();
    assert_eq!(scripts.len(), 1);
    assert_eq!(scripts[0].0, "docker_context");
}
