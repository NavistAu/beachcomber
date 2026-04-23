use beachcomber::config::{Config, parse_duration};
use std::time::Duration;

#[test]
fn default_config() {
    let config = Config::default();
    assert_eq!(
        config.daemon.log_level, "info",
        "Default log level should be info"
    );
    assert!(
        config.daemon.socket_path.is_none(),
        "Default socket path should be None"
    );
}

#[test]
fn parse_minimal_toml() {
    let toml_str = "";
    let config: Config = toml::from_str(toml_str).unwrap();
    assert_eq!(config.daemon.log_level, "info");
}

#[test]
fn parse_daemon_section() {
    let toml_str = r#"
[daemon]
log_level = "debug"
socket_path = "/tmp/test.sock"
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert_eq!(config.daemon.log_level, "debug");
    assert_eq!(config.daemon.socket_path.as_deref(), Some("/tmp/test.sock"));
}

#[test]
fn parse_lifecycle_section() {
    let toml_str = r#"
[lifecycle]
cache_lifespan = "1m"
failure_reattempts = 5
failure_backoff_interval = "2s"
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert_eq!(config.lifecycle.cache_lifespan, "1m");
    assert_eq!(
        config.lifecycle.cache_lifespan_duration(),
        std::time::Duration::from_secs(60)
    );
    assert_eq!(config.lifecycle.failure_reattempts, 5);
    assert_eq!(config.lifecycle.failure_backoff_interval, "2s");
}

#[test]
fn parse_lifecycle_backwards_compat() {
    let toml_str = r#"
[lifecycle]
grace_period_secs = 60
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert_eq!(config.lifecycle.cache_lifespan, "60s");
    assert_eq!(
        config.lifecycle.cache_lifespan_duration(),
        std::time::Duration::from_secs(60)
    );
}

#[test]
fn parse_provider_override() {
    let toml_str = r#"
[providers.battery]
command = ""
invalidation = { poll = "10s" }
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let battery = config.providers.get("battery").unwrap();
    assert_eq!(
        battery.invalidation.as_ref().unwrap().poll,
        Some("10s".to_string())
    );
}

#[test]
fn socket_path_resolves_xdg_default() {
    let config = Config::default();
    let path = config.resolve_socket_path();
    assert!(
        path.to_string_lossy().contains("beachcomber"),
        "Socket path should include 'beachcomber': {path:?}",
    );
    assert!(
        path.to_string_lossy().ends_with("sock"),
        "Socket path should end with 'sock': {path:?}",
    );
}

#[test]
fn socket_path_override() {
    let mut config = Config::default();
    config.daemon.socket_path = Some("/tmp/custom.sock".to_string());
    let path = config.resolve_socket_path();
    assert_eq!(
        path.to_string_lossy(),
        "/tmp/custom.sock",
        "Explicit socket path should be used"
    );
}

#[test]
fn log_path_resolves_xdg() {
    let config = Config::default();
    let path = config.resolve_log_path();
    assert!(
        path.to_string_lossy().contains("beachcomber"),
        "Log path should include 'beachcomber': {path:?}",
    );
}

#[test]
fn parse_duration_variants() {
    assert_eq!(parse_duration("500ms"), None);
    assert_eq!(parse_duration("5s"), Some(Duration::from_secs(5)));
    assert_eq!(parse_duration("2m"), Some(Duration::from_secs(120)));
    assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
    assert_eq!(parse_duration("30"), Some(Duration::from_secs(30)));
    assert_eq!(parse_duration(""), None);
}

#[test]
fn per_provider_backoff_overrides() {
    let toml_str = r#"
[lifecycle]
cache_lifespan = "30s"
failure_reattempts = 3
failure_backoff_interval = "1s"

[providers.my_api]
command = "curl http://example.com"
cache_lifespan = "2m"
failure_reattempts = 5
failure_backoff_interval = "2s"
poll_idle_interval = "30s"
"#;
    let config: Config = toml::from_str(toml_str).unwrap();

    // Per-provider overrides
    assert_eq!(
        config.resolve_cache_lifespan("my_api"),
        Duration::from_secs(120)
    );
    assert_eq!(config.resolve_failure_reattempts("my_api"), 5);
    assert_eq!(
        config.resolve_failure_backoff_interval("my_api"),
        Duration::from_secs(2)
    );
    assert_eq!(
        config.resolve_poll_idle_interval("my_api"),
        Some(Duration::from_secs(30))
    );

    // Unknown provider falls back to lifecycle defaults
    assert_eq!(
        config.resolve_cache_lifespan("unknown"),
        Duration::from_secs(30)
    );
    assert_eq!(config.resolve_failure_reattempts("unknown"), 3);
    assert_eq!(
        config.resolve_failure_backoff_interval("unknown"),
        Duration::from_secs(1)
    );
    assert_eq!(config.resolve_poll_idle_interval("unknown"), None);
}

#[test]
fn poll_live_interval_overrides_poll_secs() {
    let toml_str = r#"
[providers.my_api]
command = "echo test"
poll_secs = 10
poll_live_interval = "5s"
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    // poll_live_interval takes precedence
    assert_eq!(config.resolve_poll_live_interval("my_api"), Some(5));
}

#[test]
fn poll_secs_still_works() {
    let toml_str = r#"
[providers.my_api]
command = "echo test"
poll_secs = 10
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert_eq!(config.resolve_poll_live_interval("my_api"), Some(10));
}

#[test]
fn script_provider_per_field_scope_overrides_provider_default() {
    let toml = r#"
[providers.example]
type = "script"
command = "echo hello"
scope = "path"

[providers.example.fields.branch]
type = "string"

[providers.example.fields.status]
type = "string"
scope = "global"
"#;
    let config: Config = toml::from_str(toml).unwrap();
    let p = config.providers.get("example").expect("provider present");

    let branch_scope = beachcomber::config::resolve_field_scope(p, "branch");
    let status_scope = beachcomber::config::resolve_field_scope(p, "status");

    assert_eq!(branch_scope, beachcomber::provider::FieldScope::PathScoped);
    assert_eq!(status_scope, beachcomber::provider::FieldScope::Global);
}

#[test]
fn script_provider_legacy_simple_fields_inherit_provider_scope() {
    let toml = r#"
[providers.legacy]
type = "script"
command = "echo hi"
scope = "path"
fields = { branch = "string", dirty = "bool" }
"#;
    let config: Config = toml::from_str(toml).unwrap();
    let p = config.providers.get("legacy").expect("provider present");
    assert_eq!(
        beachcomber::config::resolve_field_scope(p, "branch"),
        beachcomber::provider::FieldScope::PathScoped
    );
    assert_eq!(
        beachcomber::config::resolve_field_scope(p, "dirty"),
        beachcomber::provider::FieldScope::PathScoped
    );
}

#[test]
fn script_provider_default_scope_is_global_when_not_declared() {
    let toml = r#"
[providers.noscope]
type = "script"
command = "echo hi"
fields = { branch = "string" }
"#;
    let config: Config = toml::from_str(toml).unwrap();
    let p = config.providers.get("noscope").expect("provider present");
    assert_eq!(
        beachcomber::config::resolve_field_scope(p, "branch"),
        beachcomber::provider::FieldScope::Global
    );
}

#[test]
fn lifecycle_config_accepts_new_lifecycle_fields() {
    let toml = r#"
[lifecycle]
poll_interval = "60s"
poll_live_count = 12
fsevents_reinstate = false
cache_lifespan = "30s"
failure_reattempts = 3
failure_backoff_interval = "1s"
"#;
    let config: beachcomber::config::Config = toml::from_str(toml).unwrap();
    assert_eq!(config.lifecycle.poll_interval, "60s");
    assert_eq!(config.lifecycle.poll_live_count, 12);
    assert!(!config.lifecycle.fsevents_reinstate);
}

#[test]
fn resolve_poll_interval_uses_provider_override() {
    let toml = r#"
[lifecycle]
poll_interval = "60s"
poll_live_count = 12
fsevents_reinstate = false
cache_lifespan = "30s"
failure_reattempts = 3
failure_backoff_interval = "1s"

[providers.git]
poll_interval = "10s"
"#;
    let config: beachcomber::config::Config = toml::from_str(toml).unwrap();
    assert_eq!(
        config.resolve_poll_interval("git"),
        std::time::Duration::from_secs(10)
    );
    assert_eq!(
        config.resolve_poll_interval("unknown"),
        std::time::Duration::from_secs(60)
    );
}

#[test]
fn resolve_poll_live_count_uses_provider_override() {
    let toml = r#"
[lifecycle]
poll_interval = "60s"
poll_live_count = 12
fsevents_reinstate = false
cache_lifespan = "30s"
failure_reattempts = 3
failure_backoff_interval = "1s"

[providers.git]
poll_live_count = 24
"#;
    let config: beachcomber::config::Config = toml::from_str(toml).unwrap();
    assert_eq!(config.resolve_poll_live_count("git"), 24);
    assert_eq!(config.resolve_poll_live_count("unknown"), 12);
}

#[test]
fn resolve_fsevents_reinstate_uses_provider_override() {
    let toml = r#"
[lifecycle]
poll_interval = "60s"
poll_live_count = 12
fsevents_reinstate = false
cache_lifespan = "30s"
failure_reattempts = 3
failure_backoff_interval = "1s"

[providers.mise]
fsevents_reinstate = true
"#;
    let config: beachcomber::config::Config = toml::from_str(toml).unwrap();
    assert!(config.resolve_fsevents_reinstate("mise"));
    assert!(!config.resolve_fsevents_reinstate("unknown"));
}
