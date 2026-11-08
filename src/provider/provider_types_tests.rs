use crate::provider::Value;

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
    let v = Value::Float(1.5);
    assert_eq!(v.as_text(), "1.5", "Float value should render as text");
}

#[test]
fn value_serializes_to_json() {
    let v = Value::String("main".to_string());
    let json = serde_json::to_value(&v).unwrap();
    assert_eq!(
        json,
        serde_json::json!("main"),
        "String value should serialize as JSON string"
    );
}

#[test]
fn value_int_serializes_to_json() {
    let v = Value::Int(42);
    let json = serde_json::to_value(&v).unwrap();
    assert_eq!(
        json,
        serde_json::json!(42),
        "Int value should serialize as JSON number"
    );
}

#[test]
fn from_json_null_is_absent() {
    assert_eq!(Value::from_json(&serde_json::Value::Null), None);
    assert_eq!(
        Value::from_json(&serde_json::json!("")),
        Some(Value::String(String::new())),
        "an empty string is a value"
    );
}

#[test]
fn from_json_drops_null_members_and_elements() {
    let v = Value::from_json(&serde_json::json!({
        "a": null,
        "arr": [1, null, 3],
        "o": {"x": null, "y": 2}
    }))
    .unwrap();
    let Value::Object(top) = v else {
        panic!("object")
    };
    assert!(!top.contains_key("a"));
    // A null element is absent, and the list closes up behind it.
    assert_eq!(
        top.get("arr"),
        Some(&Value::Array(vec![Value::Int(1), Value::Int(3)]))
    );
    let Some(Value::Object(o)) = top.get("o") else {
        panic!("o")
    };
    assert_eq!(o.len(), 1);
    assert_eq!(o.get("y"), Some(&Value::Int(2)));
}

#[test]
fn from_json_object_skips_null_members() {
    let map = serde_json::json!({"a": null, "b": 1});
    let r = crate::provider::SourceResult::from_json_object(map.as_object().unwrap());
    assert!(!r.fields.contains_key("a"));
    assert_eq!(r.fields.get("b"), Some(&Value::Int(1)));
}
