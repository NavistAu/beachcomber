use crate::provider::{
    FieldSchema, FieldScope, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};
use std::fs;

pub struct UptimeProvider;

impl Provider for UptimeProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "uptime".to_string(),
            fields: vec![
                FieldSchema {
                    name: "seconds".to_string(),
                    field_type: FieldType::Int,
                    scope: FieldScope::Global,
                },
                FieldSchema {
                    name: "days".to_string(),
                    field_type: FieldType::Int,
                    scope: FieldScope::Global,
                },
                FieldSchema {
                    name: "hours".to_string(),
                    field_type: FieldType::Int,
                    scope: FieldScope::Global,
                },
                FieldSchema {
                    name: "minutes".to_string(),
                    field_type: FieldType::Int,
                    scope: FieldScope::Global,
                },
            ],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 60,
                floor_secs: 10,
            },
            global: true,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Option<ProviderResult> {
        let contents = fs::read_to_string("/proc/uptime").ok()?;
        let uptime_secs = contents.split_whitespace().next()?.parse::<f64>().ok()? as i64;

        let days = uptime_secs / 86400;
        let hours = (uptime_secs % 86400) / 3600;
        let minutes = (uptime_secs % 3600) / 60;

        let mut result = ProviderResult::new();
        result.insert("seconds", Value::Int(uptime_secs));
        result.insert("days", Value::Int(days));
        result.insert("hours", Value::Int(hours));
        result.insert("minutes", Value::Int(minutes));
        Some(result)
    }
}
