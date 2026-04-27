use beachcomber::config::{ExternalSourceConfig, HttpProviderConfig};
use beachcomber::provider::Provider;
use beachcomber::provider::SourceScope;
use beachcomber::provider::http::HttpProvider;

#[test]
fn http_provider_metadata() {
    let config = HttpProviderConfig {
        url: "https://example.com/api".to_string(),
        ..Default::default()
    };
    let p = HttpProvider::new("test_http", config);
    let meta = p.metadata();
    assert_eq!(meta.name, "test_http");
    assert_eq!(meta.sources.len(), 1);
    assert_eq!(meta.sources[0].scope, SourceScope::Global);
}

#[test]
fn http_provider_env_expansion() {
    // Scope TEST_BEACHCOMBER_VAR to this test body; temp_env restores via Drop.
    temp_env::with_var("TEST_BEACHCOMBER_VAR", Some("expanded_value"), || {
        let config = HttpProviderConfig {
            url: "https://example.com/${TEST_BEACHCOMBER_VAR}".to_string(),
            ..Default::default()
        };
        let p = HttpProvider::new("env_test", config);
        let meta = p.metadata();
        assert_eq!(meta.name, "env_test");
    });
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

// ── Phase 4 multi-source HTTP tests ─────────────────────────────────────────

#[test]
fn multi_source_http_from_toml() {
    let toml_str = r#"
[providers.weather]
backend = "http"
default_timeout = "5s"

[providers.weather.current]
url = "https://api.example.com/v1/weather/current"
type = "poll"
scope = "global"
poll_interval = "10m"
poll_count = 3
fields = [
  { name = "temp_c", type = "float" },
  { name = "summary", type = "string" },
]
failback_count = 3
failback_interval = "10m"

[providers.weather.forecast]
url = "https://api.example.com/v1/weather/forecast"
type = "poll"
scope = "global"
poll_interval = "1h"
poll_count = 2
"#;
    let config: beachcomber::config::Config = toml::from_str(toml_str).unwrap();

    // Legacy http_providers() must not pick this up.
    let legacy = config.http_providers();
    assert!(
        legacy.iter().all(|(n, _)| n != "weather"),
        "multi-source http provider must not appear in legacy http_providers()"
    );

    let multi = config.multi_http_providers().expect("parses without error");
    assert_eq!(multi.len(), 1);
    let (name, default_timeout, sources) = &multi[0];
    assert_eq!(name, "weather");
    assert_eq!(default_timeout.as_deref(), Some("5s"));
    assert_eq!(sources.len(), 2);

    let current = sources
        .iter()
        .find(|s| s.name == "current")
        .expect("current source");
    let forecast = sources
        .iter()
        .find(|s| s.name == "forecast")
        .expect("forecast source");

    assert_eq!(
        current.url.as_deref(),
        Some("https://api.example.com/v1/weather/current")
    );
    assert_eq!(current.poll_count, Some(3));
    assert_eq!(
        current.fields.as_deref().map(|f| f.len()),
        Some(2),
        "current has 2 declared fields"
    );

    assert_eq!(forecast.poll_interval.as_deref(), Some("1h"));
}

#[test]
fn multi_source_http_with_sources_constructor() {
    let current = ExternalSourceConfig {
        name: "current".to_string(),
        url: Some("https://api.example.com/current".to_string()),
        strategy_type: Some("poll".to_string()),
        scope: Some("global".to_string()),
        poll_interval: Some("10m".to_string()),
        poll_count: Some(3),
        ..Default::default()
    };
    let p = HttpProvider::with_sources("weather", vec![current]);
    let meta = p.metadata();
    assert_eq!(meta.name, "weather");
    assert_eq!(meta.sources.len(), 1);
    assert_eq!(meta.sources[0].name, "current");
    assert_eq!(meta.sources[0].scope, SourceScope::Global);
}

#[test]
fn multi_source_http_missing_url_errors() {
    let toml_str = r#"
[providers.broken]
backend = "http"

[providers.broken.nurl]
type = "poll"
scope = "global"
poll_interval = "10m"
"#;
    let config: beachcomber::config::Config = toml::from_str(toml_str).unwrap();
    let result = config.multi_http_providers();
    assert!(result.is_err(), "missing url must produce an error");
    assert!(result.unwrap_err().contains("url"), "error mentions 'url'");
}

#[test]
fn multi_source_http_config_does_not_appear_in_legacy() {
    // A provider with `backend = "http"` should not be returned by legacy http_providers().
    let toml_str = r#"
[providers.newstyle]
backend = "http"

[providers.newstyle.endpoint]
url = "https://example.com/api"
type = "poll"
scope = "global"
poll_interval = "60s"
poll_count = 5

[providers.oldstyle]
type = "http"
url = "https://example.com/old"
"#;
    let config: beachcomber::config::Config = toml::from_str(toml_str).unwrap();
    let legacy = config.http_providers();
    assert_eq!(legacy.len(), 1, "only oldstyle in legacy http_providers");
    assert_eq!(legacy[0].0, "oldstyle");

    let multi = config.multi_http_providers().expect("parses");
    assert_eq!(multi.len(), 1, "only newstyle in multi_http_providers");
    assert_eq!(multi[0].0, "newstyle");
}
