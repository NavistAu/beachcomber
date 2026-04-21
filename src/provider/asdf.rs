use crate::provider::{
    FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata, ProviderResult, Value,
};
use std::collections::HashMap;
use std::path::Path;

pub struct AsdfProvider;

impl Provider for AsdfProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "asdf".to_string(),
            fields: vec![FieldSchema {
                name: "tools".to_string(),
                field_type: FieldType::Object,
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
        let tools: HashMap<String, Value> = content
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .filter_map(|line| {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    Some((parts[0].to_string(), Value::String(parts[1].to_string())))
                } else {
                    None
                }
            })
            .collect();

        let mut result = ProviderResult::new();
        result.insert("tools", Value::Object(tools));
        Some(result)
    }
}
