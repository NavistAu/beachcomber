use beachcomber::provider::hostname::HostnameProvider;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::user::UserProvider;

#[test]
fn registry_register_and_get() {
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(HostnameProvider)).unwrap();
    assert!(
        registry.provider_metadata("hostname").is_some(),
        "Should find registered provider"
    );
}

#[test]
fn registry_get_missing() {
    let registry = ProviderRegistry::new();
    assert!(
        registry.provider_metadata("nonexistent").is_none(),
        "Should return None for unknown provider"
    );
}

#[test]
fn registry_list_providers() {
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(HostnameProvider)).unwrap();
    registry.register(Box::new(UserProvider)).unwrap();
    let name_strings = registry.list();
    let mut names: Vec<&str> = name_strings.iter().map(|s| s.as_str()).collect();
    names.sort();
    assert!(names.contains(&"hostname"), "Should contain hostname");
    assert!(names.contains(&"user"), "Should contain user");
}

#[test]
fn registry_with_defaults_has_builtins() {
    let registry = ProviderRegistry::with_defaults();
    assert!(
        registry.provider_metadata("hostname").is_some(),
        "Should have hostname provider"
    );
    assert!(
        registry.provider_metadata("user").is_some(),
        "Should have user provider"
    );
}

#[test]
fn registry_source_lookup() {
    let registry = ProviderRegistry::with_defaults();
    let source = registry.source("hostname", "host");
    assert!(
        source.is_some(),
        "hostname provider should have 'host' source"
    );
}

#[test]
fn registry_execute_via_source() {
    let registry = ProviderRegistry::with_defaults();
    let source = registry.source("hostname", "host").unwrap();
    let result = source.execute(None);
    assert!(
        !result.fields.get("name").unwrap().as_text().is_empty(),
        "Hostname source should return a non-empty name"
    );
}

#[test]
fn registry_metadata() {
    let registry = ProviderRegistry::with_defaults();
    let meta = registry.provider_metadata("hostname").unwrap();
    assert_eq!(meta.name, "hostname");
}

#[test]
fn registry_has_non_virtual_blocks_store() {
    let registry = ProviderRegistry::with_defaults();
    assert!(registry.has_non_virtual("hostname"));
    assert!(!registry.has_non_virtual("nonexistent"));
}

#[test]
fn registry_source_for_field() {
    let registry = ProviderRegistry::with_defaults();
    // hostname.host source declares the "name" field
    assert_eq!(
        registry.source_for_field("hostname", "name"),
        Some("host"),
        "field 'name' on hostname should belong to source 'host'"
    );
}
