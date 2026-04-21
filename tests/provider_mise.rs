use beachcomber::provider::Provider;
use beachcomber::provider::Value;
use beachcomber::provider::mise::MiseProvider;
use tempfile::TempDir;

fn mise_toml_dir(tools: &[(&str, &str)]) -> TempDir {
    let d = tempfile::tempdir().unwrap();
    let mut body = String::from("[tools]\n");
    for (k, v) in tools {
        body.push_str(&format!("{k} = \"{v}\"\n"));
    }
    std::fs::write(d.path().join("mise.toml"), body).unwrap();
    d
}

#[test]
fn mise_tools_field_type_is_object() {
    use beachcomber::provider::FieldType;
    let meta = MiseProvider.metadata();
    let project = meta.fields.iter().find(|f| f.name == "project").unwrap();
    let global = meta.fields.iter().find(|f| f.name == "global").unwrap();
    assert!(matches!(project.field_type, FieldType::Object));
    assert!(matches!(global.field_type, FieldType::Object));
}

#[test]
fn mise_execute_returns_object_map() {
    let d = mise_toml_dir(&[("node", "20.11.0"), ("python", "3.12.1")]);
    let result = MiseProvider.execute(Some(d.path().to_str().unwrap()));
    // Skip if mise isn't installed.
    let Some(result) = result else {
        return;
    };
    match result.get("project") {
        Some(Value::Object(map)) => {
            assert!(map.contains_key("node"));
            assert!(map.contains_key("python"));
        }
        _ => panic!("project must be a Value::Object"),
    }
}
