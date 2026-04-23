use crate::provider::{
    FieldSchema, FieldScope, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};
use std::path::Path;
use std::process::Command;

pub struct DirenvProvider;

impl Provider for DirenvProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "direnv".to_string(),
            fields: vec![
                FieldSchema {
                    name: "status".to_string(),
                    field_type: FieldType::String,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "allowed".to_string(),
                    field_type: FieldType::Bool,
                    scope: FieldScope::PathScoped,
                },
            ],
            invalidation: InvalidationStrategy::Watch {
                patterns: vec![".envrc".to_string()],
                fallback_poll_secs: Some(30),
            },
            global: false,
        }
    }

    fn execute(&self, path: Option<&str>) -> Vec<(Option<String>, ProviderResult)> {
        let Some(path) = path else {
            return Vec::new();
        };
        let path_owned = path.to_string();
        let dir = Path::new(path);
        if !dir.join(".envrc").exists() {
            return Vec::new();
        }

        let allowed = Command::new("direnv")
            .args(["status"])
            .current_dir(dir)
            .output()
            .ok()
            .map(|o| {
                let stdout = String::from_utf8_lossy(&o.stdout);
                stdout.contains("Found RC allowed true")
            })
            .unwrap_or(false);

        let status = if allowed { "loaded" } else { "blocked" };

        let mut result = ProviderResult::new();
        result.insert("status", Value::String(status.to_string()));
        result.insert("allowed", Value::Bool(allowed));
        vec![(Some(path_owned), result)]
    }
}
