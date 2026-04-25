use beachcomber::provider::library::parse_library_metadata_for_test;
use beachcomber::provider::{InvalidationStrategy, KeepAlive, SourceScope};

#[test]
fn library_metadata_parses_per_field_scope() {
    // With the new trait, per-field scope is expressed through source-level scope.
    // A library declaring "global": false means the source's scope is PathScoped.
    // Per-field "scope" keys in the JSON are ignored; scope is source-level only.
    let json = r#"{
        "name": "libtest",
        "fields": {
            "branch": {"type": "string", "scope": "path"},
            "status": {"type": "string", "scope": "global"}
        },
        "invalidation": {"poll": "30s"},
        "global": false
    }"#;
    let meta = parse_library_metadata_for_test("libtest", json).expect("parses");
    assert_eq!(meta.name, "libtest");
    assert_eq!(meta.sources.len(), 1);
    // With global=false, scope is PathScoped.
    assert_eq!(meta.sources[0].scope, SourceScope::PathScoped);
    // Fields should be present.
    let field_names: Vec<&str> = meta.sources[0].fields.iter().map(|f| f.name.as_str()).collect();
    assert!(field_names.contains(&"branch"));
    assert!(field_names.contains(&"status"));
}

#[test]
fn library_metadata_legacy_global_bool_applies_default_scope() {
    let json = r#"{
        "name": "liblegacy",
        "fields": {"value": "string"},
        "global": true,
        "invalidation": {"poll": "30s"}
    }"#;
    let meta = parse_library_metadata_for_test("liblegacy", json).expect("parses");
    assert_eq!(meta.sources[0].scope, SourceScope::Global);
    let field_names: Vec<&str> = meta.sources[0].fields.iter().map(|f| f.name.as_str()).collect();
    assert!(field_names.contains(&"value"));
}

#[test]
fn library_metadata_legacy_global_false_is_pathscoped() {
    let json = r#"{
        "name": "libpath",
        "fields": {"branch": "string"},
        "global": false,
        "invalidation": {"poll": "30s"}
    }"#;
    let meta = parse_library_metadata_for_test("libpath", json).expect("parses");
    assert_eq!(meta.sources[0].scope, SourceScope::PathScoped);
    let field_names: Vec<&str> = meta.sources[0].fields.iter().map(|f| f.name.as_str()).collect();
    assert!(field_names.contains(&"branch"));
}

#[test]
fn library_metadata_mixed_explicit_and_legacy_default() {
    let json = r#"{
        "name": "libmix",
        "fields": {
            "a": {"type": "string", "scope": "path"},
            "b": "string"
        },
        "global": true,
        "invalidation": {"poll": "30s"}
    }"#;
    let meta = parse_library_metadata_for_test("libmix", json).expect("parses");
    // global=true → scope is Global
    assert_eq!(meta.sources[0].scope, SourceScope::Global);
    let field_names: Vec<&str> = meta.sources[0].fields.iter().map(|f| f.name.as_str()).collect();
    assert!(field_names.contains(&"a"));
    assert!(field_names.contains(&"b"));
}

#[test]
fn library_metadata_once_becomes_pure_watch_global_never() {
    // Legacy "once": true maps to Watch + Global + KeepAlive::Never.
    let json = r#"{
        "name": "libonce",
        "fields": {"result": "string"},
        "invalidation": {"once": true}
    }"#;
    let meta = parse_library_metadata_for_test("libonce", json).expect("parses");
    let src = &meta.sources[0];
    assert_eq!(src.scope, SourceScope::Global);
    assert!(matches!(src.invalidation, InvalidationStrategy::Watch { .. }));
    assert!(matches!(src.keep_alive, KeepAlive::Never));
}

// ── Phase 4 library backend C ABI extension tests ───────────────────────────

#[test]
fn multi_library_providers_from_toml() {
    // Parse a `backend = "library"` config block with a source-override sub-table.
    let toml_str = r#"
[providers.mygpu]
backend = "library"
library_path = "/usr/local/lib/libbeachcomber-gpu.dylib"

[providers.mygpu.usage]
poll_interval = "5s"
poll_count = 6
"#;
    let config: beachcomber::config::Config = toml::from_str(toml_str).unwrap();

    // Legacy library_providers() must not pick this up.
    let legacy = config.library_providers();
    assert!(
        legacy.iter().all(|(n, _)| n != "mygpu"),
        "multi-source library provider must not appear in legacy library_providers()"
    );

    let multi = config.multi_library_providers().expect("parses without error");
    assert_eq!(multi.len(), 1);
    let (name, lib_path, overrides) = &multi[0];
    assert_eq!(name, "mygpu");
    assert_eq!(lib_path, "/usr/local/lib/libbeachcomber-gpu.dylib");
    assert_eq!(overrides.len(), 1);

    let usage_ov = &overrides[0];
    assert_eq!(usage_ov.name, "usage");
    assert_eq!(usage_ov.poll_interval.as_deref(), Some("5s"));
    assert_eq!(usage_ov.poll_count, Some(6));
}

#[test]
fn multi_library_providers_missing_library_path_errors() {
    let toml_str = r#"
[providers.nopath]
backend = "library"
"#;
    let config: beachcomber::config::Config = toml::from_str(toml_str).unwrap();
    let result = config.multi_library_providers();
    assert!(result.is_err(), "missing library_path must produce an error");
    assert!(
        result.unwrap_err().contains("library_path"),
        "error mentions 'library_path'"
    );
}

#[test]
fn library_abi_detection_multi_source_metadata_json_name() {
    // The metadata JSON for bc_source_metadata may carry a "name" field.
    // parse_library_metadata_for_test uses this for single-source legacy;
    // in multi-source the name is set by the library's source declaration.
    let json_with_name = r#"{
        "name": "usage",
        "fields": {"gpu_pct": "float"},
        "invalidation": {"poll": "5s"},
        "global": true
    }"#;
    let meta = parse_library_metadata_for_test("mygpu", json_with_name).expect("parses");
    assert_eq!(meta.sources[0].name, "usage");
    assert_eq!(meta.sources[0].scope, SourceScope::Global);
    let field_names: Vec<&str> =
        meta.sources[0].fields.iter().map(|f| f.name.as_str()).collect();
    assert!(field_names.contains(&"gpu_pct"));
}
