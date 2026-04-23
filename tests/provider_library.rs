use beachcomber::provider::FieldScope;
use beachcomber::provider::library::parse_library_metadata_for_test;

#[test]
fn library_metadata_parses_per_field_scope() {
    let json = r#"{
        "name": "libtest",
        "fields": {
            "branch": {"type": "string", "scope": "path"},
            "status": {"type": "string", "scope": "global"}
        },
        "invalidation": {"poll": "30s"}
    }"#;
    let meta = parse_library_metadata_for_test("libtest", json).expect("parses");
    assert_eq!(meta.field_scope("branch"), Some(FieldScope::PathScoped));
    assert_eq!(meta.field_scope("status"), Some(FieldScope::Global));
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
    assert_eq!(meta.field_scope("value"), Some(FieldScope::Global));
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
    assert_eq!(meta.field_scope("branch"), Some(FieldScope::PathScoped));
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
    // "a" has explicit scope "path" → PathScoped.
    // "b" is bare string → inherits top-level global=true → Global.
    assert_eq!(meta.field_scope("a"), Some(FieldScope::PathScoped));
    assert_eq!(meta.field_scope("b"), Some(FieldScope::Global));
}
