use beachcomber::config::{Config, parse_duration};
use std::time::Duration;

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
fsevent_lifespan = "300s"
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
    assert_eq!(ov.fsevent_lifespan.as_deref(), Some("300s"));
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
fn fsevent_lifespan_forbidden_for_global_fsevent() {
    // fsevent_lifespan on scope=global + type=fsevent → error (pure-watch global never decays).
    let toml_str = r#"
[providers.hostname.host]
type = "fsevent"
scope = "global"
fsevent_lifespan = "300s"
"#;
    // This should fail during source_override() validation
    let config: Config = toml::from_str(toml_str).unwrap();
    let result = config.source_override("hostname", "host");
    assert!(
        result.is_err(),
        "fsevent_lifespan on global fsevent must error"
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
    use beachcomber::config::detect_deprecated_keys;

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
    let providers = config.script_providers();
    let (_, p) = providers
        .iter()
        .find(|(n, _)| n == "legacy")
        .expect("legacy present");
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
    let providers = config.script_providers();
    let (_, p) = providers
        .iter()
        .find(|(n, _)| n == "noscope")
        .expect("noscope present");
    assert_eq!(
        beachcomber::config::resolve_field_scope(p, "branch"),
        beachcomber::provider::FieldScope::Global
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
fsevent_lifespan = "300s"
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

#[test]
fn config_parses_virtual_field_expressions() {
    let toml = r#"
[providers.terraform]
virtual.workspace = "env.MY_TF_WORKSPACE or cache.terraform.workspace"

[providers.python]
virtual.version = "env.PYENV_VERSION or cache.python.venv_version"
"#;
    let config: Config = toml::from_str(toml).expect("valid toml");
    let fields = config.virtual_fields();
    assert_eq!(
        fields
            .get(&("terraform".to_string(), "workspace".to_string()))
            .map(|s| s.as_str()),
        Some("env.MY_TF_WORKSPACE or cache.terraform.workspace"),
    );
    assert_eq!(
        fields
            .get(&("python".to_string(), "version".to_string()))
            .map(|s| s.as_str()),
        Some("env.PYENV_VERSION or cache.python.venv_version"),
    );
}

#[test]
fn config_source_knobs_not_confused_with_virtual_fields() {
    // Source knobs (poll_interval etc.) are siblings of virtual, not inside it.
    // They must NOT appear in virtual_fields() output.
    let toml = r#"
[providers.terraform]
poll_interval = 30
virtual.workspace = "env.TF_WORKSPACE or cache.terraform.workspace"
"#;
    let config: Config = toml::from_str(toml).expect("valid toml");
    let fields = config.virtual_fields();
    assert!(
        fields.contains_key(&("terraform".to_string(), "workspace".to_string())),
        "virtual.workspace must be present"
    );
    // poll_interval is a sibling of the virtual sub-table, not inside it — not returned.
    assert!(
        !fields.contains_key(&("terraform".to_string(), "poll_interval".to_string())),
        "poll_interval must not appear in virtual_fields"
    );
}

#[test]
fn validate_providers_ignores_virtual_subtable() {
    // A built-in provider that has a `virtual` sub-table must NOT produce validation
    // errors. The daemon skips the `virtual` sub-table — it is client-side only.
    //
    // Note: TOML requires [providers.git] before [providers.git.diff] to avoid a
    // "cannot redefine table" error. virtual.branch_display is set as a dotted key
    // inside [providers.git] before the diff sub-table header.
    let toml_str = r#"
[providers.terraform]
virtual.workspace = "env.TF_WORKSPACE or cache.terraform.workspace"

[providers.aws]
virtual.region = "env.AWS_REGION or env.AWS_DEFAULT_REGION or cache.aws_profiles[env.AWS_PROFILE or \"default\"].region"

[providers.git]
virtual.branch_display = "env.GIT_BRANCH or git.branch"

[providers.git.diff]
type = "poll"
poll_interval = "30s"
poll_count = 12
"#;
    let config: Config = toml::from_str(toml_str).expect("valid toml");
    let known_providers = vec![
        "terraform".to_string(),
        "aws".to_string(),
        "git".to_string(),
    ];
    let mut known_sources = std::collections::HashMap::new();
    known_sources.insert(
        "git".to_string(),
        vec!["diff".to_string(), "refs".to_string(), "status".to_string()],
    );

    let (_, errors) = config.validate_providers(&known_providers, &known_sources);
    assert!(
        errors.is_empty(),
        "virtual sub-table must not produce validation errors: {:?}",
        errors
    );

    // Also verify virtual_fields() still returns the expressions.
    let fields = config.virtual_fields();
    assert_eq!(
        fields
            .get(&("terraform".to_string(), "workspace".to_string()))
            .map(|s| s.as_str()),
        Some("env.TF_WORKSPACE or cache.terraform.workspace"),
        "virtual_fields() must still return terraform.workspace"
    );
    assert_eq!(
        fields
            .get(&("git".to_string(), "branch_display".to_string()))
            .map(|s| s.as_str()),
        Some("env.GIT_BRANCH or git.branch"),
        "virtual_fields() must still return git.branch_display"
    );
}

#[test]
fn path_expressions_parsed_from_config() {
    let toml_str = r#"
[providers.kubecontext]
path = "env.KUBECONFIG or '~/.kube/config'"
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let exprs = config.path_expressions();
    assert_eq!(
        exprs.get("kubecontext").map(|s| s.as_str()),
        Some("env.KUBECONFIG or '~/.kube/config'"),
        "path_expressions() must return the kubecontext path expression"
    );
}

#[test]
fn path_key_does_not_trip_provider_validation() {
    // [providers.kubecontext] with a `path` string key must not cause validation errors.
    // `path` is a scalar and is skipped by the source-block validation loop.
    let toml_str = r#"
[providers.kubecontext]
path = "env.KUBECONFIG or '~/.kube/config'"
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    let known_providers = vec!["kubecontext".to_string()];
    let known_sources = std::collections::HashMap::new();
    let (warnings, errors) = config.validate_providers(&known_providers, &known_sources);
    assert!(
        errors.is_empty(),
        "path key on provider must not produce validation errors: {:?}",
        errors
    );
    assert!(
        warnings.is_empty(),
        "path key on provider must not produce validation warnings: {:?}",
        warnings
    );
}
