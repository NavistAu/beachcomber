use crate::provider::{
    FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};
use std::process::Command;

pub struct KubecontextProvider;

impl Provider for KubecontextProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "kubecontext".to_string(),
            fields: vec![
                FieldSchema { name: "context".to_string(), field_type: FieldType::String },
                FieldSchema { name: "namespace".to_string(), field_type: FieldType::String },
            ],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 30,
                floor_secs: 5,
            },
            global: true,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Option<ProviderResult> {
        let context = Command::new("kubectl")
            .args(["config", "current-context"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())?;

        let namespace = Command::new("kubectl")
            .args(["config", "view", "--minify", "--output", "jsonpath={..namespace}"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                let ns = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if ns.is_empty() { "default".to_string() } else { ns }
            })
            .unwrap_or_else(|| "default".to_string());

        let mut result = ProviderResult::new();
        result.insert("context", Value::String(context));
        result.insert("namespace", Value::String(namespace));
        Some(result)
    }
}
