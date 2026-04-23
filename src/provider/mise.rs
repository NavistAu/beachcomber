use crate::provider::{
    FieldSchema, FieldScope, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

pub struct MiseProvider;

impl Provider for MiseProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "mise".to_string(),
            fields: vec![
                FieldSchema {
                    name: "project".to_string(),
                    field_type: FieldType::Object,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "global".to_string(),
                    field_type: FieldType::Object,
                    scope: FieldScope::PathScoped,
                },
            ],
            invalidation: InvalidationStrategy::Watch {
                patterns: vec![".mise.toml".to_string(), "mise.toml".to_string()],
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

        // Check for mise config files
        let has_config = dir.join("mise.toml").exists() || dir.join(".mise.toml").exists();
        if !has_config {
            return Vec::new();
        }

        // Run mise with JSON output to get source info
        let Some(output) = Command::new("mise")
            .args(["ls", "--current", "--json"])
            .current_dir(dir)
            .output()
            .ok()
            .filter(|o| o.status.success())
        else {
            return Vec::new();
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value = match serde_json::from_str(&stdout) {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };
        let obj = match parsed.as_object() {
            Some(o) => o,
            None => return Vec::new(),
        };

        let global_config_dir = std::env::var("XDG_CONFIG_HOME")
            .or_else(|_| std::env::var("HOME").map(|h| format!("{h}/.config")))
            .map(|c| Path::new(&c).join("mise").to_string_lossy().to_string())
            .unwrap_or_default();

        let mut project_tools: HashMap<String, Value> = HashMap::new();
        let mut global_tools: HashMap<String, Value> = HashMap::new();

        for (tool_name, versions) in obj {
            let Some(arr) = versions.as_array() else {
                continue;
            };
            for entry in arr {
                let version = match entry.get("version").and_then(|v| v.as_str()) {
                    Some(v) => v,
                    None => continue,
                };
                let source_path = entry
                    .get("source")
                    .and_then(|s| s.get("path"))
                    .and_then(|p| p.as_str())
                    .unwrap_or("");

                let is_global = source_path.starts_with(&global_config_dir);

                if is_global {
                    global_tools.insert(tool_name.clone(), Value::String(version.to_string()));
                } else {
                    project_tools.insert(tool_name.clone(), Value::String(version.to_string()));
                }
            }
        }

        let mut result = ProviderResult::new();
        result.insert("project", Value::Object(project_tools));
        result.insert("global", Value::Object(global_tools));
        vec![(Some(path_owned), result)]
    }
}
