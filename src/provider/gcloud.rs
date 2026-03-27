use crate::provider::{
    FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};
use std::process::Command;

pub struct GcloudProvider;

impl Provider for GcloudProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "gcloud".to_string(),
            fields: vec![
                FieldSchema { name: "project".to_string(), field_type: FieldType::String },
                FieldSchema { name: "account".to_string(), field_type: FieldType::String },
            ],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 60,
                floor_secs: 10,
            },
            global: true,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Option<ProviderResult> {
        let project = Command::new("gcloud")
            .args(["config", "get-value", "project"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty() && s != "(unset)")?;

        let account = Command::new("gcloud")
            .args(["config", "get-value", "account"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();

        let mut result = ProviderResult::new();
        result.insert("project", Value::String(project));
        result.insert("account", Value::String(account));
        Some(result)
    }
}
