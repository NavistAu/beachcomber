use crate::provider::{
    FailbackConfig, FieldSchema, Schedule, Source, SourceMetadata, SourceResult, Value,
};

fn fake_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "main".into(),
        fields: vec![FieldSchema {
            name: "value".into(),
        }],
        schedule: Schedule {
            failback: FailbackConfig {
                reattempts: 3,
                interval_secs: 30,
            },
            ..Schedule::poll(30, 2)
        },
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

#[test]
fn a_source_that_declares_no_root_is_the_instance_at_home() {
    assert!(!FakeSource.has_root());
    assert_eq!(FakeSource.root("/some/directory"), Ok(None));
}

#[test]
fn provider_execute_returns_result() {
    let result = FakeSource.execute(None).unwrap();
    assert_eq!(
        result.fields.get("value").unwrap().as_text(),
        "hello",
        "Execute should return the expected value"
    );
}

#[test]
fn invalidation_strategy_poll() {
    let schedule = &FakeSource.metadata().schedule;
    assert!(!schedule.watches(), "Expected Poll invalidation strategy");
    assert_eq!(schedule.poll.map(|p| p.interval_secs), Some(30));
}
