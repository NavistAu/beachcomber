use shellstate::config::Config;

#[test]
fn default_config() {
    let config = Config::default();
    assert_eq!(config.daemon.log_level, "info", "Default log level should be info");
    assert!(config.daemon.socket_path.is_none(), "Default socket path should be None");
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
grace_period_secs = 60
eviction_timeout_secs = 900
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert_eq!(config.lifecycle.grace_period_secs, 60);
    assert_eq!(config.lifecycle.eviction_timeout_secs, 900);
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
    assert_eq!(battery.invalidation.as_ref().unwrap().poll, Some("10s".to_string()));
}

#[test]
fn socket_path_resolves_xdg_default() {
    let config = Config::default();
    let path = config.resolve_socket_path();
    assert!(
        path.to_string_lossy().contains("shellstate"),
        "Socket path should include 'shellstate': {:?}",
        path
    );
    assert!(
        path.to_string_lossy().ends_with("sock"),
        "Socket path should end with 'sock': {:?}",
        path
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
        path.to_string_lossy().contains("shellstate"),
        "Log path should include 'shellstate': {:?}",
        path
    );
}
