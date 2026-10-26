use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    Source, SourceMetadata, SourceResult, Value,
};

pub struct UptimeProvider;

impl Provider for UptimeProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "uptime".into(),
            sources: vec![time_source_metadata()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(UptimeTime)]
    }
}

fn time_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "time".into(),
        fields: vec![
            FieldSchema {
                name: "seconds".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "days".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "hours".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "minutes".into(),
                field_type: FieldType::Int,
            },
        ],
        invalidation: InvalidationStrategy::Poll {
            interval_secs: 60,
            count: 2,
        },
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 30,
        },
        fsevents_reinstate: false,
        decay: None,
        budget: crate::scheduler::lifecycle::DEFAULT_BUDGET,
        timeout: crate::scheduler::lifecycle::DEFAULT_TIMEOUT,
        nearest: false,
    }
}

struct UptimeTime;

impl Source for UptimeTime {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(time_source_metadata)
    }

    fn execute(&self, _path: Option<&str>) -> Result<SourceResult, String> {
        let uptime_secs = crate::plugin::sys::uptime_secs()?;
        let days = uptime_secs / 86400;
        let hours = (uptime_secs % 86400) / 3600;
        let minutes = (uptime_secs % 3600) / 60;

        let mut result = SourceResult::new();
        result.insert("seconds", Value::Int(uptime_secs));
        result.insert("days", Value::Int(days));
        result.insert("hours", Value::Int(hours));
        result.insert("minutes", Value::Int(minutes));
        Ok(result)
    }
}
