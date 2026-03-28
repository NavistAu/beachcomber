use beachcomber::provider::{Value, ProviderResult};
use std::collections::HashMap;

#[test]
fn value_from_string() {
    let v = Value::String("hello".to_string());
    assert_eq!(v.as_text(), "hello", "String value should render as text");
}

#[test]
fn value_from_int() {
    let v = Value::Int(42);
    assert_eq!(v.as_text(), "42", "Int value should render as text");
}

#[test]
fn value_from_bool() {
    let v = Value::Bool(true);
    assert_eq!(v.as_text(), "true", "Bool value should render as text");
}

#[test]
fn value_from_float() {
    let v = Value::Float(3.14);
    assert_eq!(v.as_text(), "3.14", "Float value should render as text");
}

#[test]
fn value_serializes_to_json() {
    let v = Value::String("main".to_string());
    let json = serde_json::to_value(&v).unwrap();
    assert_eq!(json, serde_json::json!("main"), "String value should serialize as JSON string");
}

#[test]
fn value_int_serializes_to_json() {
    let v = Value::Int(42);
    let json = serde_json::to_value(&v).unwrap();
    assert_eq!(json, serde_json::json!(42), "Int value should serialize as JSON number");
}

#[test]
fn provider_result_single_field() {
    let mut fields = HashMap::new();
    fields.insert("name".to_string(), Value::String("myhost".to_string()));
    let result = ProviderResult { fields };
    assert_eq!(
        result.get("name").unwrap().as_text(),
        "myhost",
        "Should retrieve single field"
    );
}

#[test]
fn provider_result_missing_field() {
    let result = ProviderResult { fields: HashMap::new() };
    assert!(result.get("missing").is_none(), "Missing field should return None");
}

#[test]
fn provider_result_to_json_object() {
    let mut fields = HashMap::new();
    fields.insert("name".to_string(), Value::String("myhost".to_string()));
    fields.insert("short".to_string(), Value::String("myh".to_string()));
    let result = ProviderResult { fields };
    let json = result.to_json();
    assert_eq!(json["name"], serde_json::json!("myhost"));
    assert_eq!(json["short"], serde_json::json!("myh"));
}

#[test]
fn provider_result_to_kv_text() {
    let mut fields = HashMap::new();
    fields.insert("name".to_string(), Value::String("myhost".to_string()));
    let result = ProviderResult { fields };
    let text = result.to_kv_text();
    assert_eq!(text, "name=myhost\n", "Should format as key=value lines");
}
