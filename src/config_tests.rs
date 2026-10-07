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

#[test]
fn fsevent_lifespan_is_a_config_error() {
    // The Active window is interval x count; fsevent_lifespan was a second spelling of it.
    let toml_str = r#"
[providers.git.refs]
type = "fsevent"
scope = "path"
fsevent_lifespan = "300s"
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let err = config.source_override("git", "refs").unwrap_err();
    assert!(
        err.contains("unknown key 'fsevent_lifespan'"),
        "error names the unknown key: {err:?}"
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
    // They are `[providers."<p>:template"]` data now (design §3.9, outcome
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
                "unrecognised block [providers.{p}.virtual]: templates live in [providers.\"{p}:template\"]"
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
fn facet_tables_extracts_only_valid_facet_namespaces() {
    let layer: toml::Value = toml::from_str(
        r#"
[daemon]
log_level = "debug"

[providers.git]
enabled = true

[providers."python:template"]
venv_name = "{{ python.venv }}"

[providers."python:notes"]
why = "here"

[providers."a:b:c"]
nope = "malformed, left to validation"
"#,
    )
    .unwrap();

    let mut found: Vec<&str> = Config::facet_tables(&layer)
        .into_iter()
        .map(|(ns, _)| ns)
        .collect();
    found.sort();
    assert_eq!(
        found,
        vec!["python:notes".to_string(), "python:template".to_string()],
        "plain providers and malformed facet keys are not facet tables"
    );

    let table = Config::facet_tables(&layer)
        .into_iter()
        .find(|(ns, _)| *ns == "python:template")
        .map(|(_, t)| t)
        .unwrap();
    assert_eq!(table["venv_name"].as_str(), Some("{{ python.venv }}"));
}

#[test]
fn settings_only_strips_exactly_the_facet_tables() {
    let composed: toml::Value = toml::from_str(
        r#"
[daemon]
log_level = "debug"

[providers.git]
enabled = true

[providers."git:template"]
branch = "{{ git.branch }}"
"#,
    )
    .unwrap();

    let settings = Config::settings_only(composed);
    assert!(
        settings["providers"].get("git").is_some(),
        "a plain provider block is a setting and survives"
    );
    assert!(
        settings["providers"].get("git:template").is_none(),
        "a facet table is not a setting and is stripped"
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

// ── Facet-table validation ───────────────────────────────────────────────────

fn facet_validation(toml_str: &str) -> (Vec<String>, Vec<String>) {
    let config: Config = toml::from_str(toml_str).expect("valid toml");
    config.validate_providers(&["git".to_string()], &std::collections::HashMap::new())
}

#[test]
fn facet_key_with_two_colons_is_an_error() {
    let (_, errors) = facet_validation("[providers.\"a:b:c\"]\nx = \"y\"\n");
    assert!(
        errors.iter().any(|e| e.contains("a:b:c")),
        "expected an error naming the malformed key, got {errors:?}"
    );
}

#[test]
fn facet_named_after_a_metadata_suffix_is_an_error() {
    let (_, errors) = facet_validation("[providers.\"x:age\"]\nn = \"1\"\n");
    assert!(
        errors.iter().any(|e| e.contains("x:age")),
        "expected an error naming the reserved facet name, got {errors:?}"
    );
}

#[test]
fn a_settings_key_in_a_facet_table_is_an_error() {
    let (_, errors) = facet_validation("[providers.\"x:template\"]\nenabled = false\n");
    assert!(
        errors.iter().any(|e| e.contains("enabled")),
        "expected an error naming 'enabled', got {errors:?}"
    );

    let (_, errors) = facet_validation("[providers.\"x:template\"]\npoll_interval = \"30s\"\n");
    assert!(
        errors.iter().any(|e| e.contains("poll_interval")),
        "a source knob is a settings key too, got {errors:?}"
    );
}

#[test]
fn a_dotted_key_under_a_template_facet_is_an_error() {
    let (_, errors) = facet_validation("[providers.\"x:template\"]\n\"a.b\" = \"{{ y }}\"\n");
    assert!(
        errors.iter().any(|e| e.contains("a.b")),
        "expected an error naming the dotted key, got {errors:?}"
    );

    let (_, errors) = facet_validation(
        "[providers.\"x:template\"]\n[providers.\"x:template\".group]\n\"c.d\" = \"{{ y }}\"\n",
    );
    assert!(
        errors.iter().any(|e| e.contains("group.c.d")),
        "the check reaches nested tables, got {errors:?}"
    );
}

#[test]
fn a_non_string_leaf_under_a_template_facet_is_a_warning() {
    let (warnings, errors) = facet_validation("[providers.\"x:template\"]\nwidth = 12\n");
    assert!(
        errors.is_empty(),
        "a bad leaf does not stop the daemon: {errors:?}"
    );
    assert!(
        warnings.iter().any(|w| w.contains("width")),
        "expected a warning naming the leaf, got {warnings:?}"
    );
}

#[test]
fn a_non_string_leaf_under_a_non_template_facet_is_data() {
    let (warnings, errors) = facet_validation("[providers.\"x:notes\"]\nwidth = 12\n");
    assert!(errors.is_empty(), "{errors:?}");
    assert!(
        warnings.is_empty(),
        "only a template's leaves must be value expressions: {warnings:?}"
    );
}

#[test]
fn a_facet_table_is_not_read_as_a_source_block() {
    // `[providers."git:template"]` must not be validated as `[providers.git.<source>]`
    // — the facet check runs first and skips the per-source loop entirely.
    let (warnings, errors) = facet_validation(
        "[providers.\"git:template\"]\n[providers.\"git:template\".diff]\nsummary = \"{{ git.diff }}\"\n",
    );
    assert!(errors.is_empty(), "{errors:?}");
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn a_dotted_key_is_an_error_in_every_facet_not_only_template() {
    // The cache refuses a dotted key whatever the facet, so without this check
    // a non-template facet would silently lose its whole layer.
    let (_, errors) = facet_validation("[providers.\"x:notes\"]\n\"a.b\" = 1\n");
    assert!(
        errors.iter().any(|e| e.contains("a.b")),
        "expected an error naming the dotted key, got {errors:?}"
    );
}

#[test]
fn a_settings_key_is_reported_once_not_twice() {
    // `enabled = false` under `:template` is a settings key (an error) and a
    // boolean leaf (a warning). Reporting both makes the same mistake look like
    // two, and the second one is not actionable.
    let (warnings, errors) = facet_validation("[providers.\"x:template\"]\nenabled = false\n");
    assert_eq!(
        errors.len(),
        1,
        "expected exactly one error, got {errors:?}"
    );
    assert!(errors[0].contains("enabled"), "{errors:?}");
    assert!(
        warnings.is_empty(),
        "a key already reported as a settings key must not also warn: {warnings:?}"
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
fn rejects_sub_second_ms() {
    assert_eq!(parse_duration("0ms"), None);
    assert_eq!(parse_duration("500ms"), None);
    assert_eq!(parse_duration("1ms"), None);
    assert_eq!(parse_duration("999ms"), None);
}

#[test]
fn rejects_non_whole_second_ms() {
    assert_eq!(parse_duration("1500ms"), None);
    assert_eq!(parse_duration("2500ms"), None);
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
