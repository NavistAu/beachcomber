use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::hostname::HostnameProvider;
use beachcomber::provider::user::UserProvider;

#[test]
fn registry_register_and_get() {
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(HostnameProvider));
    assert!(registry.get("hostname").is_some(), "Should find registered provider");
}

#[test]
fn registry_get_missing() {
    let registry = ProviderRegistry::new();
    assert!(registry.get("nonexistent").is_none(), "Should return None for unknown provider");
}

#[test]
fn registry_list_providers() {
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(HostnameProvider));
    registry.register(Box::new(UserProvider));
    let name_strings = registry.list();
    let mut names: Vec<&str> = name_strings.iter().map(|s| s.as_str()).collect();
    names.sort();
    assert_eq!(names, vec!["hostname", "user"]);
}

#[test]
fn registry_with_defaults_has_builtins() {
    let registry = ProviderRegistry::with_defaults();
    assert!(registry.get("hostname").is_some(), "Should have hostname provider");
    assert!(registry.get("user").is_some(), "Should have user provider");
}

#[test]
fn registry_execute_provider() {
    let registry = ProviderRegistry::with_defaults();
    let provider = registry.get("hostname").unwrap();
    let result = provider.execute(None).unwrap();
    assert!(
        !result.get("name").unwrap().as_text().is_empty(),
        "Hostname provider should return a non-empty name"
    );
}

#[test]
fn registry_metadata() {
    let registry = ProviderRegistry::with_defaults();
    let provider = registry.get("hostname").unwrap();
    let meta = provider.metadata();
    assert_eq!(meta.name, "hostname");
}
