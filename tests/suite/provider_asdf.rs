use beachcomber::provider::asdf::AsdfProvider;
use beachcomber::provider::{FieldType, Provider};

#[test]
fn asdf_tools_field_type_is_object() {
    let meta = AsdfProvider.metadata();
    // The sentinel field <tool> is declared in the source metadata
    let src = &meta.sources[0];
    // asdf uses a single String-typed sentinel field named "<tool>"
    assert_eq!(src.fields.len(), 1);
    assert!(matches!(src.fields[0].field_type, FieldType::String));
}

#[test]
fn asdf_tools_emitted_as_flat_fields_not_object() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join(".tool-versions"),
        "node 20.11.0\npython 3.12.1\n",
    )
    .unwrap();
    let sources = AsdfProvider.sources();
    let result = sources[0].execute(Some(d.path().to_str().unwrap()));

    // Fields should be flat: result.fields["node"] = "20.11.0"
    assert!(
        !result.fields.contains_key("tools"),
        "must not emit a nested 'tools' object; got keys: {:?}",
        result.fields.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        result.fields.get("node").unwrap().as_text(),
        "20.11.0",
        "node should be a flat top-level field"
    );
    assert_eq!(result.fields.get("python").unwrap().as_text(), "3.12.1");
}

#[test]
fn asdf_global_fallback_from_home_tool_versions() {
    // Simulate a directory with no .tool-versions and a fake HOME with one.
    let home_dir = tempfile::tempdir().unwrap();
    std::fs::write(home_dir.path().join(".tool-versions"), "ruby 3.2.0\n").unwrap();

    // Project dir: no .tool-versions, walk exhausts, falls back to HOME.
    let project_dir = tempfile::tempdir().unwrap();

    let result = temp_env::with_var("HOME", Some(home_dir.path().to_str().unwrap()), || {
        let sources = AsdfProvider.sources();
        sources[0].execute(Some(project_dir.path().to_str().unwrap()))
    });

    assert_eq!(
        result.fields.get("ruby").unwrap().as_text(),
        "3.2.0",
        "global ~/.tool-versions should be used when no local .tool-versions found"
    );
}

#[test]
fn asdf_existing_object_test_updated() {
    // After the fix, Value::Object must NOT appear — tools are flat fields.
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join(".tool-versions"),
        "node 20.11.0\npython 3.12.1\n",
    )
    .unwrap();
    let sources = AsdfProvider.sources();
    let result = sources[0].execute(Some(d.path().to_str().unwrap()));
    assert!(!result.fields.contains_key("tools"), "no 'tools' object");
    assert!(result.fields.contains_key("node"));
    assert!(result.fields.contains_key("python"));
}
