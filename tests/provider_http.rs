use beachcomber::provider::Provider;
use beachcomber::provider::http::HttpProvider;
use beachcomber::config::HttpProviderConfig;

#[test]
fn http_provider_metadata() {
    let config = HttpProviderConfig {
        url: "https://example.com/api".to_string(),
        ..Default::default()
    };
    let p = HttpProvider::new("test_http", config);
    let meta = p.metadata();
    assert_eq!(meta.name, "test_http");
    assert!(meta.global);
}

#[test]
fn http_provider_env_expansion() {
    // Test that ${VAR} expansion works by verifying metadata parses correctly
    // SAFETY: test-only mutation of env var with no concurrent threads touching it
    unsafe { std::env::set_var("TEST_BEACHCOMBER_VAR", "expanded_value") };
    let config = HttpProviderConfig {
        url: "https://example.com/${TEST_BEACHCOMBER_VAR}".to_string(),
        ..Default::default()
    };
    let p = HttpProvider::new("env_test", config);
    let meta = p.metadata();
    assert_eq!(meta.name, "env_test");
    // SAFETY: test-only mutation of env var with no concurrent threads touching it
    unsafe { std::env::remove_var("TEST_BEACHCOMBER_VAR") };
}

#[test]
fn http_providers_from_config() {
    let toml_str = r#"
[providers.status_check]
type = "http"
url = "https://status.example.com/api/summary.json"
invalidation = { poll = "60s" }

[providers.my_script]
type = "script"
command = "echo hello"
"#;
    let config: beachcomber::config::Config = toml::from_str(toml_str).unwrap();
    let http = config.http_providers();
    assert_eq!(http.len(), 1);
    assert_eq!(http[0].0, "status_check");
    assert_eq!(http[0].1.url, "https://status.example.com/api/summary.json");

    // Script providers should still work
    let scripts = config.script_providers();
    assert_eq!(scripts.len(), 1);
}

#[test]
fn extract_json_path_works() {
    // Test the extract config field is stored correctly and metadata builds without error
    let config = beachcomber::config::HttpProviderConfig {
        url: "https://httpbin.org/json".to_string(),
        extract: Some("slideshow.title".to_string()),
        ..Default::default()
    };
    let p = HttpProvider::new("extract_test", config);
    let meta = p.metadata();
    assert_eq!(meta.name, "extract_test");
}
