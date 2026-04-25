use beachcomber::provider::library::parse_library_metadata_for_test;
use beachcomber::provider::SourceScope;

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
    use beachcomber::provider::{InvalidationStrategy, KeepAlive};
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
