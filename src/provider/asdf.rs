use crate::provider::{
    FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata, ProviderResult, Value,
};
use std::path::Path;

pub struct AsdfProvider;

impl Provider for AsdfProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "asdf".to_string(),
            fields: vec![FieldSchema {
                name: "tools".to_string(),
                field_type: FieldType::String,
            }],
            invalidation: InvalidationStrategy::Watch {
                patterns: vec![".tool-versions".to_string()],
                fallback_poll_secs: Some(30),
            },
            global: false,
        }
    }

    fn execute(&self, path: Option<&str>) -> Option<ProviderResult> {
        let path = path?;
        let dir = Path::new(path);
        let tool_versions = dir.join(".tool-versions");

        if !tool_versions.exists() {
            return None;
        }

        let content = std::fs::read_to_string(&tool_versions).ok()?;
        let tools: Vec<String> = content
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
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
