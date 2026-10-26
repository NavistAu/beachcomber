use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    Source, SourceMetadata, SourceResult, Value,
};

fn fake_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "main".into(),
        fields: vec![FieldSchema {
            name: "value".into(),
            field_type: FieldType::String,
        }],
        invalidation: InvalidationStrategy::Poll {
            interval_secs: 30,
            count: 2,
        },
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 30,
        },
        decay: None,
        budget: crate::scheduler::lifecycle::DEFAULT_BUDGET,
        timeout: crate::scheduler::lifecycle::DEFAULT_TIMEOUT,
        nearest: false,
    }
}

struct FakeSource;
impl Source for FakeSource {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(fake_source_metadata)
    }
    fn execute(&self, _path: Option<&str>) -> Result<SourceResult, String> {
        let mut result = SourceResult::new();
        result.insert("value", Value::String("hello".to_string()));
        Ok(result)
    }
}

struct FakeProvider;

impl Provider for FakeProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "fake".into(),
            sources: vec![fake_source_metadata()],
        }
    }
    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(FakeSource)]
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
    assert_eq!(meta.sources.len(), 1);
    assert_eq!(meta.sources[0].fields.len(), 1, "Should have one field");
    assert_eq!(meta.sources[0].fields[0].name, "value");
}

#[test]
fn a_source_that_declares_no_root_is_the_instance_at_home() {
    let sources = FakeProvider.sources();
    assert!(!sources[0].has_root());
    assert_eq!(sources[0].root("/some/directory"), Ok(None));
}

#[test]
fn provider_execute_returns_result() {
    let p = FakeProvider;
    let sources = p.sources();
    let result = sources[0].execute(None).unwrap();
    assert_eq!(
        result.fields.get("value").unwrap().as_text(),
        "hello",
        "Execute should return the expected value"
    );
}

#[test]
fn invalidation_strategy_poll() {
    let p = FakeProvider;
    match p.metadata().sources[0].invalidation {
        InvalidationStrategy::Poll { interval_secs, .. } => {
            assert_eq!(interval_secs, 30);
        }
        _ => panic!("Expected Poll invalidation strategy"),
    }
}
