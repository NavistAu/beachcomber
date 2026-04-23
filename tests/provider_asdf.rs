use beachcomber::provider::Provider;
use beachcomber::provider::asdf::AsdfProvider;
use beachcomber::provider::{FieldType, Value};

#[test]
fn asdf_tools_field_type_is_object() {
    let meta = AsdfProvider.metadata();
    let tools = meta.fields.iter().find(|f| f.name == "tools").unwrap();
    assert!(matches!(tools.field_type, FieldType::Object));
}

#[test]
fn asdf_execute_returns_object_map() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join(".tool-versions"),
        "node 20.11.0\npython 3.12.1\n",
    )
    .unwrap();
    let (_, result) = AsdfProvider
        .execute(Some(d.path().to_str().unwrap()))
        .into_iter()
        .next()
        .unwrap();
    match result.get("tools") {
        Some(Value::Object(map)) => {
            assert_eq!(map.get("node").unwrap().as_text(), "20.11.0");
            assert_eq!(map.get("python").unwrap().as_text(), "3.12.1");
        }
        _ => panic!("tools must be a Value::Object"),
    }
}
