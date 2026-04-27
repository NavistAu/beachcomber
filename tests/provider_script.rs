use beachcomber::config::{ExternalSourceConfig, ScriptProviderConfig};
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
    assert!(
        !result.fields.is_empty(),
        "path-scoped execute should return fields"
    );
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
    assert!(
        result.fields.is_empty(),
        "Failed command should return empty SourceResult"
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

// ── Phase 4 multi-source tests ───────────────────────────────────────────────

#[test]
fn multi_source_script_provider_from_toml() {
    let toml_str = r#"
[providers.localdash]
backend = "script"

[providers.localdash.weather]
command = "/usr/local/bin/weather-cache"
type = "poll"
scope = "global"
poll_interval = "10m"
poll_count = 3
fields = [{ name = "temp", type = "float" }, { name = "summary", type = "string" }]
failback_count = 3
failback_interval = "5m"

[providers.localdash.disk]
command = "/usr/local/bin/disk-summary"
type = "poll"
scope = "global"
poll_interval = "30s"
poll_count = 6
fields = [{ name = "used_pct", type = "int" }]
"#;
    let config: beachcomber::config::Config = toml::from_str(toml_str).unwrap();

    // Legacy path should not pick this up (it has `backend` key).
    let legacy = config.script_providers();
    assert!(
        legacy.iter().all(|(n, _)| n != "localdash"),
        "multi-source provider must not appear in legacy script_providers()"
    );

    let multi = config
        .multi_script_providers()
        .expect("parses without error");
    assert_eq!(multi.len(), 1, "exactly one multi-source script provider");
    let (name, sources) = &multi[0];
    assert_eq!(name, "localdash");
    assert_eq!(sources.len(), 2, "two sources declared");

    // Sources may come back in any order (HashMap).
    let weather = sources
        .iter()
        .find(|s| s.name == "weather")
        .expect("weather source");
    let disk = sources
        .iter()
        .find(|s| s.name == "disk")
        .expect("disk source");

    assert_eq!(
        weather.command.as_deref(),
        Some("/usr/local/bin/weather-cache")
    );
    assert_eq!(weather.poll_interval.as_deref(), Some("10m"));
    assert_eq!(weather.poll_count, Some(3));
    assert_eq!(weather.failback_count, Some(3));
    assert_eq!(
        weather.fields.as_deref().map(|f| f.len()),
        Some(2),
        "weather declares 2 fields"
    );

    assert_eq!(disk.command.as_deref(), Some("/usr/local/bin/disk-summary"));
    assert_eq!(disk.poll_count, Some(6));
}

#[test]
fn multi_source_script_with_sources_constructor() {
    // ScriptProvider::with_sources builds a multi-source provider.
    let weather = ExternalSourceConfig {
        name: "weather".to_string(),
        command: Some(r#"echo '{"temp":21.5}'"#.to_string()),
        strategy_type: Some("poll".to_string()),
        scope: Some("global".to_string()),
        poll_interval: Some("10m".to_string()),
        poll_count: Some(3),
        ..Default::default()
    };
    let disk = ExternalSourceConfig {
        name: "disk".to_string(),
        command: Some(r#"echo '{"used_pct":42}'"#.to_string()),
        strategy_type: Some("poll".to_string()),
        scope: Some("global".to_string()),
        poll_interval: Some("30s".to_string()),
        poll_count: Some(6),
        ..Default::default()
    };

    let p = ScriptProvider::with_sources("localdash", vec![weather, disk]);
    let meta = p.metadata();
    assert_eq!(meta.name, "localdash");
    assert_eq!(meta.sources.len(), 2);

    let src_names: Vec<&str> = meta.sources.iter().map(|s| s.name.as_str()).collect();
    assert!(src_names.contains(&"weather"), "weather source present");
    assert!(src_names.contains(&"disk"), "disk source present");

    // Execute each source.
    let sources = p.sources();
    for s in &sources {
        let result = s.execute(None);
        assert!(
            !result.fields.is_empty(),
            "source '{}' should return fields",
            s.metadata().name
        );
    }
}

#[test]
fn multi_source_script_config_missing_command_errors() {
    let toml_str = r#"
[providers.broken]
backend = "script"

[providers.broken.nosource]
type = "poll"
scope = "global"
"#;
    let config: beachcomber::config::Config = toml::from_str(toml_str).unwrap();
    let result = config.multi_script_providers();
    assert!(result.is_err(), "missing command must produce an error");
    assert!(
        result.unwrap_err().contains("command"),
        "error mentions 'command'"
    );
}
