use beachcomber::provider::{
    FieldSchema, FieldScope, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};

struct FakeProvider;

impl Provider for FakeProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "fake".to_string(),
            fields: vec![FieldSchema {
                name: "value".to_string(),
                field_type: FieldType::String,
                scope: FieldScope::Global,
            }],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 30,
                floor_secs: 5,
            },
        }
    }

    fn execute(&self, _path: Option<&str>) -> Vec<(Option<String>, ProviderResult)> {
        let mut result = ProviderResult::new();
        result.insert("value", Value::String("hello".to_string()));
        vec![(None, result)]
    }
}

#[test]
fn provider_metadata_name() {
    let p = FakeProvider;
    assert_eq!(p.metadata().name, "fake", "Provider name should be 'fake'");
}

#[test]
fn provider_metadata_fields() {
    let p = FakeProvider;
    let meta = p.metadata();
    assert_eq!(meta.fields.len(), 1, "Should have one field");
    assert_eq!(meta.fields[0].name, "value");
}

#[test]
fn provider_metadata_is_global() {
    let p = FakeProvider;
    assert_eq!(
        p.metadata().inferred_scope(),
        FieldScope::Global,
        "Fake provider should be global"
    );
}

#[test]
fn provider_execute_returns_result() {
    let p = FakeProvider;
    let (_, result) = p.execute(None).into_iter().next().unwrap();
    assert_eq!(
        result.get("value").unwrap().as_text(),
        "hello",
        "Execute should return the expected value"
    );
}

#[test]
fn invalidation_strategy_poll() {
    let p = FakeProvider;
    match p.metadata().invalidation {
        InvalidationStrategy::Poll {
            interval_secs,
            floor_secs,
        } => {
            assert_eq!(interval_secs, 30);
            assert_eq!(floor_secs, 5);
        }
        _ => panic!("Expected Poll invalidation strategy"),
    }
}
