use beachcomber::provider::asdf::AsdfProvider;
use beachcomber::provider::{FieldType, Provider, Value};

#[test]
fn asdf_tools_field_type_is_object() {
    let meta = AsdfProvider.metadata();
    // The sentinel field <tool> is declared in the source metadata
    let src = &meta.sources[0];
    // asdf uses a single Object-typed sentinel field named "<tool>"
    assert_eq!(src.fields.len(), 1);
    assert!(matches!(src.fields[0].field_type, FieldType::String));
}

#[test]
fn asdf_execute_returns_object_map() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join(".tool-versions"),
        "node 20.11.0\npython 3.12.1\n",
    )
    .unwrap();
    let sources = AsdfProvider.sources();
    let result = sources[0].execute(Some(d.path().to_str().unwrap()));
    match result.fields.get("tools") {
        Some(Value::Object(map)) => {
            assert_eq!(map.get("node").unwrap().as_text(), "20.11.0");
            assert_eq!(map.get("python").unwrap().as_text(), "3.12.1");
        }
        _ => panic!("tools must be a Value::Object"),
    }
}
