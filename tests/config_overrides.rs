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
        registry.get("battery").is_none(),
        "Disabled provider should not be registered"
    );
    // Other providers should still exist
    assert!(registry.get("hostname").is_some());
}

#[test]
fn all_providers_registered_by_default() {
    let config = Config::default();
    let registry = ProviderRegistry::with_config(&config);
    assert!(registry.get("hostname").is_some());
    assert!(registry.get("user").is_some());
    assert!(registry.get("git").is_some());
    assert!(registry.get("battery").is_some());
}

#[test]
fn config_override_preserved() {
    let toml_str = r#"
[providers.battery]
poll_secs = 10
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let battery = config.providers.get("battery").unwrap();
    assert_eq!(
        battery
            .invalidation
            .as_ref()
            .and_then(|i| i.poll.as_deref()),
        None
    );
    // poll_secs is a separate field for overrides
    assert_eq!(battery.poll_secs, Some(10));
}
