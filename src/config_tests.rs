use crate::config::{Config, parse_duration};
use crate::provider::registry::ProviderRegistry;
use std::time::Duration;
use tempfile::TempDir;

// ── from tests/suite/config.rs ──────────

// ── Table-driven TOML-parse cases ────────────────────────────────────────────
//
// These cases all follow the same shape: parse TOML text → assert specific
// Config struct fields. Each case is a (name, toml_input, check_fn) triple.
// Cases that exercise env interaction, file I/O, method resolution, error
// paths, or multi-step logic remain as individual #[test] items below.

struct ConfigParseCase {
    name: &'static str,
    input: &'static str,
    check: fn(&Config),
}

#[test]
fn config_parse_cases() {
    let cases: &[ConfigParseCase] = &[
        ConfigParseCase {
            name: "parse_minimal_toml",
            input: "",
            check: |c| {
                assert_eq!(c.daemon.log_level, "info");
            },
        },
        ConfigParseCase {
            name: "parse_daemon_section",
            input: r#"
[daemon]
log_level = "debug"
socket_path = "/tmp/test.sock"
"#,
            check: |c| {
                assert_eq!(c.daemon.log_level, "debug");
                assert_eq!(c.daemon.socket_path.as_deref(), Some("/tmp/test.sock"));
            },
        },
        ConfigParseCase {
            name: "parse_lifecycle_section",
            input: r#"
[lifecycle]
poll_interval = "30s"
poll_live_count = 5
"#,
            check: |c| {
                assert_eq!(c.lifecycle.poll_interval, "30s");
                assert_eq!(c.lifecycle.poll_live_count, 5);
            },
        },
        ConfigParseCase {
            name: "parse_failback_section",
            input: r#"
[failback]
count = 5
interval = "2s"
"#,
            check: |c| {
                assert_eq!(c.failback.count, 5);
                assert_eq!(c.failback.interval, "2s");
            },
        },
        ConfigParseCase {
            name: "parse_lifecycle_legacy_keys_ignored",
            // failure_reattempts and failure_backoff_interval have moved to [failback].
            // The old [lifecycle] keys are silently ignored by serde; defaults still apply.
            input: r#"
[lifecycle]
cache_lifespan = "1m"
grace_period_secs = 60
failure_reattempts = 3
failure_backoff_interval = "1s"
"#,
            check: |c| {
                assert_eq!(c.lifecycle.poll_interval, "60s");
            },
        },
        ConfigParseCase {
            name: "lifecycle_config_accepts_new_fields",
            input: r#"
[lifecycle]
poll_interval = "60s"
poll_live_count = 12
fsevents_reinstate = false
"#,
            check: |c| {
                assert_eq!(c.lifecycle.poll_interval, "60s");
                assert_eq!(c.lifecycle.poll_live_count, 12);
                assert_eq!(c.lifecycle.fsevents_reinstate, Some(false));
            },
        },
        ConfigParseCase {
            name: "failback_defaults_apply_without_config",
            input: "",
            check: |c| {
                assert_eq!(c.failback.count, 3);
                assert_eq!(c.failback.interval_duration(), Duration::from_secs(1));
            },
        },
        ConfigParseCase {
            name: "legacy_cache_lifespan_parses_without_error",
            // Legacy configs with cache_lifespan should parse cleanly; key is ignored.
            input: r#"
[lifecycle]
cache_lifespan = "30s"
poll_interval = "60s"
poll_live_count = 12
"#,
            check: |c| {
                assert_eq!(c.lifecycle.poll_interval, "60s");
            },
        },
        ConfigParseCase {
            name: "legacy_poll_idle_interval_parses_without_error",
            input: r#"
[lifecycle]
poll_interval = "60s"
poll_live_count = 12

[providers.git]
poll_idle_interval = "5m"
"#,
            check: |c| {
                assert_eq!(
                    c.resolve_poll_interval("git"),
                    std::time::Duration::from_secs(60)
                );
            },
        },
        ConfigParseCase {
            name: "legacy_poll_live_interval_parses_without_error",
            // poll_live_interval is ignored; falls back to global lifecycle interval.
            input: r#"
[lifecycle]
poll_interval = "60s"
poll_live_count = 12

[providers.git]
poll_live_interval = "10s"
"#,
            check: |c| {
                assert_eq!(
                    c.resolve_poll_interval("git"),
                    std::time::Duration::from_secs(60)
                );
            },
        },
    ];

    for case in cases {
        let config: Config = toml::from_str(case.input)
            .unwrap_or_else(|e| panic!("case {}: parse failed: {}", case.name, e));
        (case.check)(&config);
    }
}

// ── Standalone tests ──────────────────────────────────────────────────────────
//
// These tests exercise Config::default(), method resolution chains, error/Err
// paths, validate_providers(), and other logic that doesn't fit a pure
// "TOML text → check parsed fields" sweep.

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
fn parse_provider_enabled_flag() {
    let toml_str = r#"
[providers.battery]
enabled = false
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert!(config.is_provider_disabled("battery"));
    assert!(!config.is_provider_disabled("hostname"));
}

#[test]
fn parse_per_source_block() {
    let toml_str = r#"
[providers.git.refs]
type = "fsevent"
scope = "path"
fsevent_patterns = [".git"]
failback_count = 3
failback_interval = "60s"
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let ov = config.source_override("git", "refs").unwrap().unwrap();
    assert_eq!(ov.strategy_type.as_deref(), Some("fsevent"));
    assert_eq!(ov.scope.as_deref(), Some("path"));
    assert_eq!(
        ov.fsevent_patterns.as_deref(),
        Some(&[".git".to_string()][..])
    );
    assert_eq!(ov.failback_count, Some(3));
    assert_eq!(ov.failback_interval.as_deref(), Some("60s"));
}

#[test]
fn parse_poll_source_block() {
    let toml_str = r#"
[providers.git.diff]
type = "poll"
scope = "path"
poll_interval = "30s"
poll_count = 12
failback_count = 3
failback_interval = "60s"
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let ov = config.source_override("git", "diff").unwrap().unwrap();
    assert_eq!(ov.strategy_type.as_deref(), Some("poll"));
    assert_eq!(ov.poll_interval.as_deref(), Some("30s"));
    assert_eq!(ov.poll_count, Some(12));
}

#[test]
fn parse_fsevent_poll_source_block() {
    let toml_str = r#"
[providers.git.status]
type = "fsevent_poll"
scope = "path"
poll_interval = "60s"
poll_count = 2
fsevent_patterns = [".git/index"]
fsevent_reinstates = true
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let ov = config.source_override("git", "status").unwrap().unwrap();
    assert_eq!(ov.strategy_type.as_deref(), Some("fsevent_poll"));
    assert_eq!(ov.poll_interval.as_deref(), Some("60s"));
    assert_eq!(ov.poll_count, Some(2));
    assert_eq!(
        ov.fsevent_patterns.as_deref(),
        Some(&[".git/index".to_string()][..])
    );
    assert_eq!(ov.fsevent_reinstates, Some(true));
}

#[test]
fn source_override_absent_returns_none() {
    let config: Config = toml::from_str("").unwrap();
    assert!(config.source_override("git", "refs").unwrap().is_none());
}

#[test]
fn socket_path_resolves_xdg_default() {
    let path = Config::default_socket_path();
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
    assert_eq!(parse_duration("500ms"), Some(Duration::from_millis(500)));
    assert_eq!(parse_duration("5s"), Some(Duration::from_secs(5)));
    assert_eq!(parse_duration("2m"), Some(Duration::from_secs(120)));
    assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
    assert_eq!(parse_duration("30"), Some(Duration::from_secs(30)));
    assert_eq!(parse_duration(""), None);
}

#[test]
fn per_source_failback_overrides() {
    let toml_str = r#"
[failback]
count = 3
interval = "1s"

[providers.my_api.main]
failback_count = 5
failback_interval = "2s"
"#;
    let config: Config = toml::from_str(toml_str).unwrap();

    // Per-source overrides
    assert_eq!(
        config.resolve_failure_reattempts_for_source("my_api", Some("main"), None),
        5
    );
    assert_eq!(
        config.resolve_failure_backoff_for_source("my_api", Some("main"), None),
        Duration::from_secs(2)
    );

    // Unknown source falls back to [failback] global defaults
    assert_eq!(
        config.resolve_failure_reattempts_for_source("my_api", Some("other"), None),
        3
    );
    assert_eq!(
        config.resolve_failure_backoff_for_source("my_api", Some("other"), None),
        Duration::from_secs(1)
    );
    // No source specified — uses failback global
    assert_eq!(config.resolve_failure_reattempts("unknown"), 3);
    assert_eq!(
        config.resolve_failure_backoff_interval("unknown"),
        Duration::from_secs(1)
    );
}

#[test]
fn per_source_poll_interval_overrides() {
    let toml_str = r#"
[lifecycle]
poll_interval = "60s"
poll_live_count = 12

[providers.git.diff]
type = "poll"
poll_interval = "10s"
poll_count = 6
"#;
    let config: Config = toml::from_str(toml_str).unwrap();

    // Per-source override
    assert_eq!(
        config.resolve_poll_interval_for_source("git", Some("diff"), None),
        Duration::from_secs(10)
    );
    assert_eq!(
        config.resolve_poll_live_count_for_source("git", Some("diff"), None),
        6
    );

    // Different source falls back to global lifecycle
    assert_eq!(
        config.resolve_poll_interval_for_source("git", Some("refs"), None),
        Duration::from_secs(60)
    );
    assert_eq!(
        config.resolve_poll_live_count_for_source("git", Some("refs"), None),
        12
    );
    // No source specified
    assert_eq!(
        config.resolve_poll_interval("unknown"),
        Duration::from_secs(60)
    );
    assert_eq!(config.resolve_poll_live_count("unknown"), 12);
}

#[test]
fn per_source_fsevents_reinstates_overrides() {
    let toml_str = r#"
[lifecycle]
fsevents_reinstate = false

[providers.mise.project]
type = "fsevent"
fsevent_reinstates = true
"#;
    let config: Config = toml::from_str(toml_str).unwrap();

    // Per-source override wins over global lifecycle setting.
    assert!(config.resolve_fsevents_reinstate_for_source("mise", Some("project"), false));
    // No per-source override; global lifecycle (false) beats the source default (true).
    assert!(!config.resolve_fsevents_reinstate_for_source("mise", Some("global"), true));
}

#[test]
fn fsevents_reinstate_falls_through_to_source_default() {
    // Neither per-source nor global is set → source-declared default wins.
    let toml_str = r#"
[lifecycle]
poll_interval = "60s"
poll_live_count = 12
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert!(config.resolve_fsevents_reinstate_for_source("mise", Some("project"), true));
    assert!(!config.resolve_fsevents_reinstate_for_source("git", Some("refs"), false));
}

#[test]
fn fsevents_reinstate_global_override_beats_source_default() {
    // Explicit global false overrides a source default of true.
    let toml_str = r#"
[lifecycle]
fsevents_reinstate = false
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert!(!config.resolve_fsevents_reinstate_for_source("mise", Some("project"), true));
}

#[test]
fn old_flat_source_knob_on_builtin_detected_by_validate() {
    // Old flat source-knob keys on a non-external-backend provider block
    // are caught by validate_providers(), not by the TOML parser.
    let toml_str = r#"
[providers.git]
poll_interval = "10s"
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let known_providers = vec!["git".to_string()];
    let known_sources: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let (_, errors) = config.validate_providers(&known_providers, &known_sources);
    assert!(
        !errors.is_empty(),
        "old flat source-knob key on built-in provider must produce a validation error"
    );
    assert!(
        errors[0].contains("poll_interval"),
        "error message should name the offending key: {:?}",
        errors
    );
    assert!(
        errors[0].contains("[providers.git.<source_name>]"),
        "error message should name the new schema shape: {:?}",
        errors
    );
}

#[test]
fn old_flat_failure_reattempts_on_builtin_detected_by_validate() {
    let toml_str = r#"
[providers.hostname]
failure_reattempts = 5
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let known_providers = vec!["hostname".to_string()];
    let known_sources = std::collections::HashMap::new();
    let (_, errors) = config.validate_providers(&known_providers, &known_sources);
    assert!(
        !errors.is_empty(),
        "failure_reattempts on builtin provider must error"
    );
    assert!(errors[0].contains("failure_reattempts"));
}

#[test]
fn external_backend_flat_keys_accepted() {
    // script/library/http providers still accept legacy flat keys until Phase 4.
    let toml_str = r#"
[providers.my_api]
type = "script"
command = "echo hi"
failure_reattempts = 5
poll_interval = "10s"
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let known_providers: Vec<String> = vec![];
    let known_sources = std::collections::HashMap::new();
    let (_, errors) = config.validate_providers(&known_providers, &known_sources);
    // External backends (with type = "script") are exempt from old-shape rejection.
    assert!(
        errors.is_empty(),
        "script provider flat keys should not produce errors: {:?}",
        errors
    );
}

#[test]
fn unknown_source_on_known_provider_warns_not_errors() {
    // Known provider, unknown source name → warn (cross-platform support).
    let toml_str = r#"
[providers.battery.upower]
type = "poll"
poll_interval = "60s"
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let known_providers = vec!["battery".to_string()];
    let mut known_sources = std::collections::HashMap::new();
    known_sources.insert("battery".to_string(), vec!["state".to_string()]);
    let (warnings, errors) = config.validate_providers(&known_providers, &known_sources);
    assert!(
        errors.is_empty(),
        "unknown source on known provider must not error: {:?}",
        errors
    );
    assert!(
        !warnings.is_empty(),
        "unknown source on known provider must produce a warning"
    );
    assert!(
        warnings[0].contains("upower"),
        "warning names the unknown source: {:?}",
        warnings
    );
    assert!(
        warnings[0].contains("state"),
        "warning lists registered sources: {:?}",
        warnings
    );
    assert!(
        warnings[0].contains("platform-conditional"),
        "warning mentions platform-conditional context: {:?}",
        warnings
    );
}

#[test]
fn strategy_key_mismatch_detected_by_validate() {
    // poll_interval on fsevent type → error.
    let toml_str = r#"
[providers.git.refs]
type = "fsevent"
fsevent_patterns = [".git"]
poll_interval = "60s"
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let known_providers: Vec<String> = vec![];
    let known_sources = std::collections::HashMap::new();
    let (_, errors) = config.validate_providers(&known_providers, &known_sources);
    assert!(
        !errors.is_empty(),
        "poll_interval on fsevent type must produce an error"
    );
    assert!(
        errors[0].contains("fsevent"),
        "error mentions strategy: {:?}",
        errors
    );
}

fn assert_unknown_key(err: &str) {
    assert!(
        err.contains("unknown key 'fsevent_lifespan'"),
        "error is the ordinary unknown-key error: {err:?}"
    );
}

#[test]
fn fsevent_lifespan_is_an_unknown_key_in_a_built_in_source_block() {
    let config: Config = toml::from_str(
        "[providers.git.refs]\ntype = \"fsevent\"\nscope = \"path\"\nfsevent_lifespan = \"300s\"\n",
    )
    .unwrap();
    assert_unknown_key(&config.source_override("git", "refs").unwrap_err());
}

#[test]
fn fsevent_lifespan_is_an_unknown_key_in_an_external_source_block_parser() {
    let table: toml::Table =
        toml::from_str("command = \"true\"\nfsevent_lifespan = \"300s\"\n").unwrap();
    assert_unknown_key(
        &crate::config::ExternalSourceConfig::from_toml_table("p.main", &table).unwrap_err(),
    );
}

#[test]
fn fsevent_keys_on_poll_type_rejected() {
    let toml_str = r#"
[providers.aws.profile]
type = "poll"
fsevent_patterns = [".aws"]
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let result = config.source_override("aws", "profile");
    assert!(result.is_err(), "fsevent_patterns on poll type must error");
}

#[test]
fn deprecated_keys_detected_in_raw_toml() {
    use crate::config::detect_deprecated_keys;

    let toml = r#"
[lifecycle]
cache_lifespan = "30s"
failure_reattempts = 3
failure_backoff_interval = "1s"

[providers.git]
poll_idle_interval = "5m"
poll_live_interval = "10s"
"#;
    let warnings = detect_deprecated_keys(toml);
    // cache_lifespan, failure_reattempts, failure_backoff_interval, poll_idle_interval, poll_live_interval
    assert!(warnings.len() >= 4);
    assert!(
        warnings
            .iter()
            .any(|w: &String| w.contains("cache_lifespan"))
    );
    assert!(
        warnings
            .iter()
            .any(|w: &String| w.contains("failure_reattempts"))
    );
    assert!(
        warnings
            .iter()
            .any(|w: &String| w.contains("failure_backoff_interval"))
    );
    assert!(
        warnings
            .iter()
            .any(|w: &String| w.contains("poll_idle_interval"))
    );
    assert!(
        warnings
            .iter()
            .any(|w: &String| w.contains("poll_live_interval"))
    );
}

#[test]
fn resolution_order_per_source_beats_source_default_beats_global() {
    // per-source block > source declared default > [lifecycle]/[failback] global
    let toml_str = r#"
[lifecycle]
poll_interval = "120s"
poll_live_count = 20

[failback]
count = 10
interval = "10s"

[providers.git.diff]
poll_interval = "5s"
poll_count = 3
failback_count = 1
failback_interval = "500ms"
"#;
    // Note: "500ms" parses as 0 (sub-second), so fallback to source_default or global.
    let config: Config = toml::from_str(toml_str).unwrap();

    // 1. Per-source block wins
    assert_eq!(
        config.resolve_poll_interval_for_source("git", Some("diff"), Some(Duration::from_secs(99))),
        Duration::from_secs(5),
        "per-source poll_interval beats source default"
    );
    assert_eq!(
        config.resolve_poll_live_count_for_source("git", Some("diff"), Some(99)),
        3,
        "per-source poll_count beats source default"
    );
    assert_eq!(
        config.resolve_failure_reattempts_for_source("git", Some("diff"), Some(99)),
        1,
        "per-source failback_count beats source default"
    );

    // 2. Source declared default beats global when no per-source block
    assert_eq!(
        config.resolve_poll_interval_for_source("git", Some("refs"), Some(Duration::from_secs(45))),
        Duration::from_secs(45),
        "source default beats global when no per-source override"
    );
    assert_eq!(
        config.resolve_poll_live_count_for_source("git", Some("refs"), Some(7)),
        7,
        "source default beats global poll_live_count"
    );
    assert_eq!(
        config.resolve_failure_reattempts_for_source("git", Some("refs"), Some(4)),
        4,
        "source default beats global failback count"
    );

    // 3. Global fallback when no per-source or source default
    assert_eq!(
        config.resolve_poll_interval_for_source("git", Some("refs"), None),
        Duration::from_secs(120),
        "global lifecycle poll_interval applies without source default"
    );
    assert_eq!(
        config.resolve_poll_live_count_for_source("git", Some("refs"), None),
        20,
        "global lifecycle poll_live_count applies without source default"
    );
    assert_eq!(
        config.resolve_failure_reattempts_for_source("git", Some("refs"), None),
        10,
        "global failback count applies without source default"
    );
}

#[test]
fn script_provider_per_field_scope_overrides_provider_default() {
    // Script providers still use flat provider-level keys for their backend config.
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
    let providers = config.script_providers();
    let (_, p) = providers
        .iter()
        .find(|(n, _)| n == "example")
        .expect("example present");

    let branch_scope = crate::config::resolve_field_scope(p, "branch");
    let status_scope = crate::config::resolve_field_scope(p, "status");

    assert_eq!(branch_scope, crate::provider::FieldScope::PathScoped);
    assert_eq!(status_scope, crate::provider::FieldScope::Global);
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
    let providers = config.script_providers();
    let (_, p) = providers
        .iter()
        .find(|(n, _)| n == "legacy")
        .expect("legacy present");
    assert_eq!(
        crate::config::resolve_field_scope(p, "branch"),
        crate::provider::FieldScope::PathScoped
    );
    assert_eq!(
        crate::config::resolve_field_scope(p, "dirty"),
        crate::provider::FieldScope::PathScoped
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
    let providers = config.script_providers();
    let (_, p) = providers
        .iter()
        .find(|(n, _)| n == "noscope")
        .expect("noscope present");
    assert_eq!(
        crate::config::resolve_field_scope(p, "branch"),
        crate::provider::FieldScope::Global
    );
}

#[test]
fn unknown_key_in_source_block_produces_error() {
    // [providers.git.refs] with an unknown key must error.
    let toml_str = r#"
[providers.git.refs]
type = "fsevent"
not_a_valid_key = "oops"
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let result = config.source_override("git", "refs");
    assert!(
        result.is_err(),
        "unknown key in source block must produce an error"
    );
}

#[test]
fn validate_providers_no_errors_for_valid_per_source_config() {
    let toml_str = r#"
[providers.git.diff]
type = "poll"
poll_interval = "30s"
poll_count = 12
failback_count = 3
failback_interval = "60s"

[providers.git.refs]
type = "fsevent"
scope = "path"
fsevent_patterns = [".git"]
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let known_providers = vec!["git".to_string()];
    let mut known_sources = std::collections::HashMap::new();
    known_sources.insert(
        "git".to_string(),
        vec!["diff".to_string(), "refs".to_string(), "status".to_string()],
    );
    let (warnings, errors) = config.validate_providers(&known_providers, &known_sources);
    assert!(warnings.is_empty(), "no warnings expected: {:?}", warnings);
    assert!(errors.is_empty(), "no errors expected: {:?}", errors);
}

// ── Phase 4 external backend TOML validation tests ──────────────────────────

#[test]
fn backend_script_source_blocks_accepted_in_validate() {
    // `backend = "script"` providers with per-source sub-tables should pass validate_providers.
    let toml_str = r#"
[providers.localdash]
backend = "script"

[providers.localdash.weather]
command = "/usr/bin/weather"
type = "poll"
scope = "global"
poll_interval = "10m"
poll_count = 3

[providers.localdash.disk]
command = "/usr/bin/disk"
type = "poll"
scope = "global"
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let known_providers: Vec<String> = vec![];
    let known_sources = std::collections::HashMap::new();
    let (_, errors) = config.validate_providers(&known_providers, &known_sources);
    assert!(
        errors.is_empty(),
        "backend = script provider must not produce validation errors: {:?}",
        errors
    );
}

#[test]
fn backend_http_source_blocks_accepted_in_validate() {
    let toml_str = r#"
[providers.myapi]
backend = "http"
default_timeout = "5s"

[providers.myapi.stats]
url = "https://api.example.com/stats"
type = "poll"
scope = "global"
poll_interval = "60s"
poll_count = 5
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let known_providers: Vec<String> = vec![];
    let known_sources = std::collections::HashMap::new();
    let (_, errors) = config.validate_providers(&known_providers, &known_sources);
    assert!(
        errors.is_empty(),
        "backend = http provider must not produce validation errors: {:?}",
        errors
    );
}

#[test]
fn backend_library_source_override_blocks_accepted_in_validate() {
    let toml_str = r#"
[providers.mygpu]
backend = "library"
library_path = "/usr/local/lib/libgpu.dylib"

[providers.mygpu.usage]
poll_interval = "5s"
poll_count = 6
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let known_providers: Vec<String> = vec![];
    let known_sources = std::collections::HashMap::new();
    let (_, errors) = config.validate_providers(&known_providers, &known_sources);
    assert!(
        errors.is_empty(),
        "backend = library provider must not produce validation errors: {:?}",
        errors
    );
}

#[test]
fn backend_script_invalid_source_key_produces_error() {
    // An unknown key in a source sub-table of an external backend must error.
    let toml_str = r#"
[providers.myprov]
backend = "script"

[providers.myprov.src]
command = "/usr/bin/cmd"
not_a_valid_key = "oops"
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let known_providers: Vec<String> = vec![];
    let known_sources = std::collections::HashMap::new();
    let (_, errors) = config.validate_providers(&known_providers, &known_sources);
    assert!(
        !errors.is_empty(),
        "unknown key in external source block must produce an error"
    );
    assert!(
        errors[0].contains("not_a_valid_key"),
        "error names the unknown key: {:?}",
        errors
    );
}

// ── Legacy flat external backend TOML validation tests ──────────────────────

#[test]
fn legacy_flat_script_provider_fields_and_invalidation_accepted_in_validate() {
    // Legacy flat shape: bare top-level `command`, no `backend` key. `fields`
    // and `invalidation` are provider-level sub-tables in this shape (see
    // ScriptProviderConfig::{fields, invalidation}), not per-source blocks —
    // validate_providers must not flag them as unrecognized source blocks.
    let toml_str = r#"
[providers.chronoclaude]
command = "comb eval '{{ scanline.stdin }}' | '/path/statusline'"
output = "text"
fields = { value = "string" }

[providers.chronoclaude.invalidation]
poll = "1s"
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let known_providers: Vec<String> = vec![];
    let known_sources = std::collections::HashMap::new();
    let (warnings, errors) = config.validate_providers(&known_providers, &known_sources);
    assert!(
        errors.is_empty(),
        "legacy flat provider's fields/invalidation sub-tables must not \
         produce validation errors: {:?}",
        errors
    );
    assert!(warnings.is_empty(), "no warnings expected: {:?}", warnings);
}

#[test]
fn legacy_flat_provider_unknown_subtable_still_errors() {
    // A genuinely unrecognized sub-table on a legacy flat provider must still
    // be rejected — the fields/invalidation carve-out must not swallow
    // everything.
    let toml_str = r#"
[providers.chronoclaude]
command = "/path/statusline"
output = "text"

[providers.chronoclaude.bogus]
not_a_real_key = "junk"
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let known_providers: Vec<String> = vec![];
    let known_sources = std::collections::HashMap::new();
    let (_, errors) = config.validate_providers(&known_providers, &known_sources);
    assert!(
        !errors.is_empty(),
        "unknown sub-table on legacy flat provider must still produce an error"
    );
}

#[test]
fn backend_script_fields_subtable_still_errors() {
    // A Phase 4 `backend = "script"` block does not use provider-level
    // `fields`/`invalidation` sub-tables — the legacy-flat carve-out must not
    // extend to explicit-backend providers, so this behaves exactly as it
    // did before the fix (no scope creep onto Phase 4 validation).
    let toml_str = r#"
[providers.localdash]
backend = "script"
fields = { value = "string" }

[providers.localdash.weather]
command = "/usr/bin/weather"
type = "poll"
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let known_providers: Vec<String> = vec![];
    let known_sources = std::collections::HashMap::new();
    let (_, errors) = config.validate_providers(&known_providers, &known_sources);
    assert!(
        !errors.is_empty(),
        "backend = script provider with a stray provider-level 'fields' sub-table \
         must still error, unchanged from pre-fix behavior"
    );
}

#[test]
fn validate_providers_rejects_a_virtual_subtable_naming_the_template_facet() {
    // `[providers.<p>.virtual]` was where 0.9 kept a provider's templates.
    // They are `[client.providers.<p>] template` data now (design §3.9, outcome
    // 15), so the old block is unrecognised — and its error says where the
    // templates went, rather than the generic per-source "unknown key"
    // any other stray sub-table gets. `virtual.env = …` is the dotted-key
    // spelling of the same table. The check runs before the provider's kind
    // is consulted, so every kind gets the same error: a namespace no
    // provider stands behind (`conda`), a builtin with a real source block
    // beside it (`git`), a `backend = "script"`, a `backend = "library"`,
    // and a legacy bare-`command` provider. Nothing else in the file is
    // wrong, so these five are the whole verdict.
    let toml_str = r#"
[providers.conda]
virtual.env = "{{ env.CONDA_DEFAULT_ENV }}"

[providers.git]
virtual.branch_display = "{{ env.GIT_BRANCH or git.branch }}"

[providers.git.diff]
type = "poll"
poll_interval = "30s"
poll_count = 12

[providers.localdash]
backend = "script"
virtual.label = "{{ localdash.weather }}"

[providers.localdash.weather]
command = "/usr/bin/weather"
type = "poll"

[providers.mygpu]
backend = "library"
library_path = "/usr/local/lib/libgpu.dylib"
virtual.label = "{{ mygpu.usage }}"

[providers.mygpu.usage]
poll_interval = "5s"

[providers.chronoclaude]
command = "/path/statusline"
output = "text"
virtual.label = "{{ chronoclaude.value }}"
"#;
    let config: Config = toml::from_str(toml_str).expect("valid toml");
    let known_providers = vec!["git".to_string()];
    let mut known_sources = std::collections::HashMap::new();
    known_sources.insert(
        "git".to_string(),
        vec!["diff".to_string(), "refs".to_string(), "status".to_string()],
    );
    let (warnings, errors) = config.validate_providers(&known_providers, &known_sources);
    let expected: Vec<String> = ["chronoclaude", "conda", "git", "localdash", "mygpu"]
        .iter()
        .map(|p| {
            format!(
                "unrecognised block [providers.{p}.virtual]: templates live in [client.providers.{p}] template.<leaf>"
            )
        })
        .collect();
    let mut sorted = errors.clone();
    sorted.sort();
    assert_eq!(sorted, expected, "errors: {errors:?}");
    assert!(warnings.is_empty(), "no warnings expected: {warnings:?}");
}

/// `path` was the path-expression setting (design §3.8, outcome 19). Nothing
/// computes an entry's path any more — a query addresses the caller's
/// directory, `--path`, or `--global` — so the key is an error that says so,
/// on a built-in and on a namespace the registry does not know alike.
#[test]
fn a_path_key_under_a_provider_is_a_validation_error() {
    let toml_str = r#"
[providers.foo]
path = "{{ env.FOOCONFIG or '~/.foo/config' }}"

[providers.git]
path = "{{ cwd }}"
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let (warnings, errors) =
        config.validate_providers(&["git".to_string()], &std::collections::HashMap::new());
    let mut sorted = errors.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        ["foo", "git"].map(|p| format!(
            "[providers.{p}]: path is not a provider setting: a query addresses the caller's \
             directory, --path, or --global"
        )),
        "errors: {errors:?}"
    );
    assert!(warnings.is_empty(), "no warnings expected: {warnings:?}");
}

// ── Layers, facet tables, and the settings/facets split (Task 6) ─────────────

fn write(dir: &std::path::Path, rel: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, body).unwrap();
    path
}

#[test]
fn layers_are_defaults_then_the_main_file_then_conf_d_in_lexical_order() {
    let tmp = tempfile::tempdir().unwrap();
    let main = write(
        tmp.path(),
        "config.toml",
        "[daemon]\nlog_level = \"debug\"\n",
    );
    let b = write(
        tmp.path(),
        "conf.d/20-b.toml",
        "[daemon]\nlog_level = \"warn\"\n",
    );
    let a = write(
        tmp.path(),
        "conf.d/10-a.toml",
        "[daemon]\nlog_level = \"trace\"\n",
    );
    let python = write(
        tmp.path(),
        "defaults/python.toml",
        "[daemon]\nlog_level = \"error\"\n",
    );
    let aws = write(
        tmp.path(),
        "defaults/aws.toml",
        "[daemon]\nlog_level = \"info\"\n",
    );

    let layers = Config::layers(
        Some(&tmp.path().join("defaults")),
        Some(&main),
        &tmp.path().join("conf.d"),
    );
    let paths: Vec<_> = layers.iter().map(|(p, _)| p.clone()).collect();
    assert_eq!(
        paths,
        vec![aws, python, main, a, b],
        "layers compose defaults lexically, then the main file, then conf.d lexically"
    );

    // Each file is parsed on its own, not merged: an early layer still holds
    // the value the later ones override.
    assert_eq!(
        layers[0].1["daemon"]["log_level"].as_str(),
        Some("info"),
        "a layer keeps its own value; composition happens later"
    );
    assert_eq!(layers[2].1["daemon"]["log_level"].as_str(), Some("debug"));
    assert_eq!(layers[4].1["daemon"]["log_level"].as_str(), Some("warn"));
}

#[test]
fn layers_skip_a_file_that_does_not_parse() {
    let tmp = tempfile::tempdir().unwrap();
    let main = write(tmp.path(), "config.toml", "[daemon]\n");
    write(
        tmp.path(),
        "conf.d/10-broken.toml",
        "this is not [ valid toml",
    );
    let ok = write(tmp.path(), "conf.d/20-ok.toml", "[lifecycle]\n");

    let paths: Vec<_> = Config::layers(None, Some(&main), &tmp.path().join("conf.d"))
        .into_iter()
        .map(|(p, _)| p)
        .collect();
    assert_eq!(paths, vec![main, ok], "the broken drop-in is skipped");
}

#[test]
fn layers_tolerate_a_missing_main_config_file() {
    let tmp = tempfile::tempdir().unwrap();
    let drop_in = write(tmp.path(), "conf.d/10-only.toml", "[daemon]\n");
    let paths: Vec<_> = Config::layers(
        Some(&tmp.path().join("defaults")),
        Some(&tmp.path().join("config.toml")),
        &tmp.path().join("conf.d"),
    )
    .into_iter()
    .map(|(p, _)| p)
    .collect();
    assert_eq!(
        paths,
        vec![drop_in],
        "a missing main file and a missing defaults directory are both nothing"
    );
}

#[test]
fn settings_only_strips_exactly_the_seeded_tables() {
    let composed: toml::Value = toml::from_str(
        r#"
[daemon]
log_level = "debug"

[daemon.providers.git.cache._]
decay = 2

[providers.git]
enabled = true

[client]
bogus = 1

[client.providers.git]
template.branch = "{{ git.branch }}"
"#,
    )
    .unwrap();

    let settings = Config::settings_only(composed);
    assert!(
        settings["client"].get("providers").is_none(),
        "[client.providers] is not a setting"
    );
    assert_eq!(
        settings["client"]["bogus"].as_integer(),
        Some(1),
        "any other [client] key is, so a change to it restarts"
    );
    assert!(
        settings["providers"].get("git").is_some(),
        "a plain provider block is a setting and survives"
    );
    assert!(
        settings["daemon"].get("providers").is_none(),
        "a decay table is not a setting, and leaves no empty shell behind"
    );
    assert_eq!(settings["daemon"]["log_level"].as_str(), Some("debug"));
}

#[test]
fn config_label_writes_home_as_tilde() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    // SAFETY: nextest gives every test its own process.
    unsafe { std::env::set_var("HOME", &home) };

    assert_eq!(
        Config::config_label(&home.join(".config/beachcomber/conf.d/10-mine.toml")),
        "config:~/.config/beachcomber/conf.d/10-mine.toml"
    );
    assert_eq!(
        Config::config_label(std::path::Path::new("/etc/beachcomber/config.toml")),
        "config:/etc/beachcomber/config.toml",
        "a path outside $HOME is written in full"
    );
}

// ── Seeded-table validation (one file at a time) ──────────────────────────────────────────────────

fn seeded_errors(toml_str: &str) -> Vec<String> {
    let layer: toml::Value = toml::from_str(toml_str).expect("valid toml");
    crate::shape::seeded::read(std::path::Path::new("/f.toml"), &layer)
        .errors
        .into_iter()
        .map(|e| {
            e.strip_prefix("config:/f.toml: ")
                .unwrap_or_else(|| panic!("an error carries its file's label: {e}"))
                .to_string()
        })
        .collect()
}

#[test]
fn unknown_key_under_client_is_an_error() {
    assert_eq!(
        seeded_errors("[client]\nbogus = 1\n"),
        vec!["unknown key 'bogus' in [client]".to_string()]
    );
    assert_eq!(
        seeded_errors("[client.providers.x]\nenabled = false\n"),
        vec!["unknown key 'enabled' in [client.providers.x]".to_string()]
    );
}

#[test]
fn a_dotted_key_under_a_template_is_an_error() {
    let errors = seeded_errors("[client.providers.x]\ntemplate.\"a.b\" = \"{{ y }}\"\n");
    assert!(errors.iter().any(|e| e.contains("'a.b'")), "{errors:?}");

    let errors = seeded_errors("[client.providers.x.template.group]\n\"c.d\" = \"{{ y }}\"\n");
    assert!(
        errors.iter().any(|e| e.contains("group.c.d")),
        "the check reaches nested tables, got {errors:?}"
    );
}

#[test]
fn a_non_string_template_leaf_is_an_error() {
    assert_eq!(
        seeded_errors("[client.providers.x]\ntemplate.width = 12\n"),
        vec![
            "[client.providers.x] template 'width' is integer, not a value expression or a group."
                .to_string()
        ]
    );
}

#[test]
fn a_non_integer_decay_is_an_error() {
    for (bad, shown) in [
        ("\"4\"", "\"4\""),
        ("0", "0"),
        ("-1", "-1"),
        ("17", "17"),
        ("1.5", "1.5"),
        ("true", "true"),
    ] {
        assert_eq!(
            seeded_errors(&format!("[daemon.providers.git.cache._]\ndecay = {bad}\n")),
            vec![format!(
                "git:cache._.decay must be an integer from 1 to 16 (got {shown})"
            )],
            "decay = {bad}"
        );
    }
    assert_eq!(
        seeded_errors("[daemon.providers._.cache._]\ndecay = 0\n"),
        vec!["*:cache._.decay must be an integer from 1 to 16 (got 0)".to_string()],
        "the abstract provider is named *"
    );
    for ok in [1, 4, 16] {
        assert!(
            seeded_errors(&format!("[daemon.providers.git.cache._]\ndecay = {ok}\n")).is_empty()
        );
    }
}

#[test]
fn only_decay_is_accepted_under_a_cache_prototype() {
    assert_eq!(
        seeded_errors("[daemon.providers.git]\nenabled = true\n"),
        vec!["unknown key 'enabled' in [daemon.providers.git]".to_string()]
    );
    assert_eq!(
        seeded_errors("[daemon.providers.git.cache]\npoll = 5\n"),
        vec!["unknown key 'poll' in [daemon.providers.git.cache]".to_string()]
    );
    assert_eq!(
        seeded_errors("[daemon.providers.git.cache._]\npoll = 5\n"),
        vec!["unknown key 'poll' in [daemon.providers.git.cache._]".to_string()]
    );
}

#[test]
fn an_old_colon_table_is_an_unknown_table() {
    for name in ["x:template", "*:cache", "a:b:c", "x:put"] {
        assert_eq!(
            seeded_errors(&format!("[providers.\"{name}\"]\nn = \"1\"\n")),
            vec![format!(
                "unknown table [providers.\"{name}\"]: templates live in [client.providers.<p>] \
                 template.<leaf>, decay in [daemon.providers.<p>.cache._]"
            )]
        );
    }
}

#[test]
fn provider_names_hold_no_colon_star_or_nothing_and_the_prototype_is_not_a_client_provider() {
    let errors = seeded_errors("[client.providers.\"a:b\"]\ntemplate.x = \"1\"\n");
    assert!(errors.iter().any(|e| e.contains("'a:b'")), "{errors:?}");
    let errors = seeded_errors("[client.providers._]\ntemplate.x = \"1\"\n");
    assert!(errors.iter().any(|e| e.contains("'_'")), "{errors:?}");
    let errors = seeded_errors("[daemon.providers.\"*\"]\n");
    assert!(errors.iter().any(|e| e.contains("'*'")), "{errors:?}");
    assert!(seeded_errors("[daemon.providers._.cache._]\ndecay = 4\n").is_empty());
}

#[test]
fn an_old_colon_table_is_not_also_read_as_a_source_block() {
    // Through the real parse: `[providers."git:template"]` with a sub-table
    // would be `git`'s source block if the colon name were not skipped, and
    // `git` is a known provider with no `diff` source declared here. The
    // unknown-table error is the layer's; the composed validation adds none.
    let text = "[providers.\"git:template\"]\n[providers.\"git:template\".diff]\nsummary = \"{{ git.diff }}\"\n";
    let config = Config::parse_str(text).unwrap();
    let (warnings, errors) =
        config.validate_providers(&["git".to_string()], &std::collections::HashMap::new());
    assert!(errors.is_empty(), "{errors:?}");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(
        seeded_errors(text),
        vec![
            "unknown table [providers.\"git:template\"]: templates live in [client.providers.<p>] \
             template.<leaf>, decay in [daemon.providers.<p>.cache._]"
                .to_string()
        ]
    );
}

#[test]
fn an_error_names_its_file_and_each_file_is_checked_on_its_own() {
    // The merge of these two is valid (the later string wins the leaf), but
    // seeding is per file, so the first file's integer is an error.
    let bad: toml::Value = toml::from_str("[client.providers.zz]\ntemplate.width = 12\n").unwrap();
    let good: toml::Value =
        toml::from_str("[client.providers.zz]\ntemplate.width = \"{{ 1 }}\"\n").unwrap();
    let errors = Config::validate_layers(&[
        (std::path::PathBuf::from("/a.toml"), bad.clone()),
        (std::path::PathBuf::from("/b.toml"), good),
        (std::path::PathBuf::from("/c.toml"), bad),
    ]);
    assert_eq!(
        errors,
        vec![
            "config:/a.toml: [client.providers.zz] template 'width' is integer, not a value \
             expression or a group."
                .to_string(),
            "config:/c.toml: [client.providers.zz] template 'width' is integer, not a value \
             expression or a group."
                .to_string(),
        ]
    );
}

#[test]
fn an_empty_key_in_a_template_group_names_the_group() {
    assert_eq!(
        seeded_errors("[client.providers.x.template.g]\n\"\" = \"e\"\n"),
        vec!["[client.providers.x] template.g has an empty key".to_string()]
    );
}

#[test]
fn an_empty_template_key_is_an_error() {
    assert_eq!(
        seeded_errors("[client.providers.x]\ntemplate.\"\" = \"e\"\n"),
        vec!["[client.providers.x] template has an empty key".to_string()]
    );
}

#[test]
fn a_file_seeds_only_what_validated() {
    let layer: toml::Value = toml::from_str(
        "[client.providers.bad]\ntemplate.n = 1\n\n[client.providers.ok]\ntemplate.n = \"{{ 1 }}\"\n\n\
         [daemon.providers.git.cache._]\ndecay = 0\n\n[daemon.providers._.cache._]\ndecay = 3\n",
    )
    .unwrap();
    let read = crate::shape::seeded::read(std::path::Path::new("/f.toml"), &layer);
    let mut namespaces: Vec<&str> = read.writes.iter().map(|(n, _)| n.as_str()).collect();
    namespaces.sort();
    assert_eq!(namespaces, ["*:cache", "ok:template"]);
    assert_eq!(read.errors.len(), 2);
}

#[test]
fn a_client_that_is_not_a_table_is_an_error() {
    assert_eq!(
        seeded_errors("client = \"oops\"\n"),
        vec!["[client] must be a table".to_string()]
    );
}

#[test]
fn settings_only_keeps_a_non_cache_sibling_as_a_setting() {
    let composed: toml::Value = toml::from_str(
        "[daemon.providers.git]\nenabled = false\n\n[daemon.providers.git.cache._]\ndecay = 2\n",
    )
    .unwrap();
    let settings = Config::settings_only(composed);
    assert_eq!(
        settings["daemon"]["providers"]["git"]["enabled"].as_bool(),
        Some(false)
    );
    assert!(
        settings["daemon"]["providers"]["git"]
            .get("cache")
            .is_none()
    );
}

// ── from tests/suite/config_overrides.rs ──────────

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

// ── from tests/suite/config_parse_duration.rs ──────────

#[test]
fn accepts_whole_seconds() {
    assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
    assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
    assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
}

#[test]
fn accepts_ms_that_are_whole_seconds() {
    assert_eq!(parse_duration("1000ms"), Some(Duration::from_secs(1)));
    assert_eq!(parse_duration("2000ms"), Some(Duration::from_secs(2)));
}

#[test]
fn parse_duration_accepts_milliseconds() {
    assert_eq!(parse_duration("50ms"), Some(Duration::from_millis(50)));
    assert_eq!(parse_duration("1ms"), Some(Duration::from_millis(1)));
    assert_eq!(parse_duration("1500ms"), Some(Duration::from_millis(1500)));
    assert_eq!(parse_duration("2000ms"), Some(Duration::from_secs(2)));
    assert_eq!(parse_duration("0ms"), None);
}

#[test]
fn parse_duration_overflow_is_none_not_a_panic() {
    assert_eq!(parse_duration("999999999999999999h"), None);
    assert_eq!(parse_duration("18446744073709551615m"), None);
}

#[test]
fn the_watchdog_keys_refuse_sub_second_values() {
    let config: Config = toml::from_str(
        "[daemon]\nwatchdog_interval = \"500ms\"\nwatchdog_threshold = \"1500ms\"\n",
    )
    .unwrap();
    assert_eq!(config.daemon.watchdog_interval_duration(), None);
    assert_eq!(config.daemon.watchdog_threshold_duration(), None);
    let config: Config =
        toml::from_str("[daemon]\nwatchdog_interval = \"2s\"\nwatchdog_threshold = \"2000ms\"\n")
            .unwrap();
    assert_eq!(
        config.daemon.watchdog_interval_duration(),
        Some(Duration::from_secs(2))
    );
    assert_eq!(
        config.daemon.watchdog_threshold_duration(),
        Some(Duration::from_secs(2))
    );
}

#[test]
fn whole_second_consumers_reject_sub_second_values() {
    use crate::scheduler::parse_duration_secs_pub as secs;
    assert_eq!(secs("500ms"), None);
    assert_eq!(secs("1500ms"), None);
    assert_eq!(secs("2000ms"), Some(2));
    assert_eq!(secs("5s"), Some(5));
}

#[test]
fn empty_or_garbage_returns_none() {
    assert_eq!(parse_duration(""), None);
    assert_eq!(parse_duration("abc"), None);
    assert_eq!(parse_duration("5x"), None);
}

// ── from tests/suite/env_file.rs ──────────

#[test]
fn loads_env_file() {
    let tmp = TempDir::new().unwrap();
    let env_path = tmp.path().join("env");
    std::fs::write(
        &env_path,
        r#"
# This is a comment
BEACHCOMBER_TEST_KEY=hello_world
BEACHCOMBER_TEST_QUOTED="quoted value"
BEACHCOMBER_TEST_SINGLE='single quoted'

# Blank lines are fine
BEACHCOMBER_TEST_NUM=42
"#,
    )
    .unwrap();

    let mut config = Config::default();
    config.daemon.env_file = Some(env_path.to_string_lossy().to_string());

    // Scope the test so load_env_file's set_var calls are rolled back via Drop,
    // regardless of assertion panics. All four vars start and end as unset.
    temp_env::with_vars(
        [
            ("BEACHCOMBER_TEST_KEY", None::<&str>),
            ("BEACHCOMBER_TEST_QUOTED", None::<&str>),
            ("BEACHCOMBER_TEST_SINGLE", None::<&str>),
            ("BEACHCOMBER_TEST_NUM", None::<&str>),
        ],
        || {
            let count = config.load_env_file();
            assert_eq!(count, 4, "Should load 4 variables");
            assert_eq!(
                std::env::var("BEACHCOMBER_TEST_KEY").unwrap(),
                "hello_world"
            );
            assert_eq!(
                std::env::var("BEACHCOMBER_TEST_QUOTED").unwrap(),
                "quoted value"
            );
            assert_eq!(
                std::env::var("BEACHCOMBER_TEST_SINGLE").unwrap(),
                "single quoted"
            );
            assert_eq!(std::env::var("BEACHCOMBER_TEST_NUM").unwrap(), "42");
        },
    );
}

#[test]
fn missing_env_file_returns_zero() {
    let mut config = Config::default();
    config.daemon.env_file = Some("/nonexistent/path/to/env".to_string());

    let count = config.load_env_file();
    assert_eq!(count, 0, "Missing file should return 0");
}

#[test]
fn no_env_file_configured_returns_zero() {
    // Config with env_file pointing to a guaranteed-nonexistent path so we
    // never fall through to ~/.config/beachcomber/env on the developer's machine.
    let tmp = TempDir::new().unwrap();
    let mut config = Config::default();
    config.daemon.env_file = Some(
        tmp.path()
            .join("nonexistent_env_file")
            .to_string_lossy()
            .to_string(),
    );
    let count = config.load_env_file();
    assert_eq!(count, 0, "nonexistent env_file should return 0");
}
