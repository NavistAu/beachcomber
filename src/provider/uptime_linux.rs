use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use std::fs;

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
        scope: SourceScope::Global,
        invalidation: InvalidationStrategy::Poll { interval_secs: 60 },
        keep_alive: KeepAlive::Polls(2),
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 30,
        },
        fsevents_reinstate: false,
    }
}

struct UptimeTime;

impl Source for UptimeTime {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(time_source_metadata)
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        let Some(contents) = fs::read_to_string("/proc/uptime").ok() else {
            return SourceResult::new();
        };
        let Some(uptime_secs) = contents
            .split_whitespace()
            .next()
            .and_then(|s| s.parse::<f64>().ok())
            .map(|f| f as i64)
        else {
            return SourceResult::new();
        };

        let days = uptime_secs / 86400;
        let hours = (uptime_secs % 86400) / 3600;
        let minutes = (uptime_secs % 3600) / 60;

        let mut result = SourceResult::new();
        result.insert("seconds", Value::Int(uptime_secs));
        result.insert("days", Value::Int(days));
        result.insert("hours", Value::Int(hours));
        result.insert("minutes", Value::Int(minutes));
        result
    }
}
