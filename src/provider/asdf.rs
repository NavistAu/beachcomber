use crate::provider::{
    FieldSchema, FieldScope, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
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
                scope: FieldScope::PathScoped,
            }],
            invalidation: InvalidationStrategy::Watch {
                patterns: vec![".tool-versions".to_string()],
                fallback_poll_secs: Some(30),
            },
        }
    }

    // Walk up from `path` to the nearest directory containing `.tool-versions`.
    // asdf itself consults the file by walking up, so matching that means
    // subdirs share a single cache entry with the project root.
    fn canonical_path(&self, path: Option<&str>) -> Option<String> {
        let p = path?;
        find_tool_versions_root(Path::new(p))
    }

    fn execute(&self, path: Option<&str>) -> Vec<(Option<String>, ProviderResult)> {
        let Some(path) = path else {
            return Vec::new();
        };
        let path_owned = path.to_string();
        let dir = Path::new(path);
        let tool_versions = dir.join(".tool-versions");

        if !tool_versions.exists() {
            return Vec::new();
        }

        let Some(content) = std::fs::read_to_string(&tool_versions).ok() else {
            return Vec::new();
        };
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
        vec![(Some(path_owned), result)]
    }
}

fn find_tool_versions_root(start: &Path) -> Option<String> {
    let mut cur: Option<&Path> = Some(start);
    while let Some(dir) = cur {
        if dir.join(".tool-versions").exists() {
            return Some(dir.to_string_lossy().to_string());
        }
        cur = dir.parent();
    }
    None
}
