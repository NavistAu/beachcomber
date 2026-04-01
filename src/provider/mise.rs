use crate::provider::{
    FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata, ProviderResult, Value,
};
use std::path::Path;
use std::process::Command;

pub struct MiseProvider;

impl Provider for MiseProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "mise".to_string(),
            fields: vec![FieldSchema {
                name: "tools".to_string(),
                field_type: FieldType::String,
            }],
            invalidation: InvalidationStrategy::Watch {
                patterns: vec![".mise.toml".to_string(), "mise.toml".to_string()],
                fallback_poll_secs: Some(30),
            },
            global: false,
        }
    }

    fn execute(&self, path: Option<&str>) -> Option<ProviderResult> {
        let path = path?;
        let dir = Path::new(path);

        // Check for mise config files
        let has_config = dir.join("mise.toml").exists() || dir.join(".mise.toml").exists();
        if !has_config {
            return None;
        }

        // Run mise to get current tool versions
        let output = Command::new("mise")
            .args(["ls", "--current", "--no-header"])
            .current_dir(dir)
            .output()
            .ok()
            .filter(|o| o.status.success())?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let tools: Vec<String> = stdout
            .lines()
            .filter_map(|line| {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    Some(format!("{}={}", parts[0], parts[1]))
                } else {
                    None
                }
            })
            .collect();

        let mut result = ProviderResult::new();
        result.insert("tools", Value::String(tools.join(",")));
        Some(result)
    }
}
