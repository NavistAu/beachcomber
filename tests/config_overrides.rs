use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;

#[test]
fn disabled_provider_not_registered() {
    let toml_str = r#"
[providers.battery]
enabled = false
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let registry = ProviderRegistry::with_config(&config);
    assert!(
        registry.provider_sources("battery").is_none(),
        "Disabled provider should not be registered"
    );
    // Other providers should still exist
    assert!(registry.provider_sources("hostname").is_some());
}

#[test]
fn all_providers_registered_by_default() {
    let config = Config::default();
    let registry = ProviderRegistry::with_config(&config);
    assert!(registry.provider_sources("hostname").is_some());
    assert!(registry.provider_sources("user").is_some());
    assert!(registry.provider_sources("git").is_some());
    assert!(registry.provider_sources("battery").is_some());
}

#[test]
fn per_source_poll_interval_override_parsed() {
    // Verifies the new per-source schema is parsed correctly.
    let toml_str = r#"
[providers.battery.state]
type = "poll"
poll_interval = "10s"
poll_count = 6
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let ov = config
        .source_override("battery", "state")
        .expect("no error")
        .expect("block present");
    assert_eq!(ov.poll_interval.as_deref(), Some("10s"));
    assert_eq!(ov.poll_count, Some(6));
}
