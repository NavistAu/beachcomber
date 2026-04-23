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
                    scope: FieldScope::Global,
                },
                FieldSchema {
                    name: "global".to_string(),
                    field_type: FieldType::Object,
                    scope: FieldScope::Global,
                },
            ],
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

        // Run mise with JSON output to get source info
        let output = Command::new("mise")
            .args(["ls", "--current", "--json"])
            .current_dir(dir)
            .output()
            .ok()
            .filter(|o| o.status.success())?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value = serde_json::from_str(&stdout).ok()?;
        let obj = parsed.as_object()?;

        let global_config_dir = std::env::var("XDG_CONFIG_HOME")
            .or_else(|_| std::env::var("HOME").map(|h| format!("{h}/.config")))
            .map(|c| Path::new(&c).join("mise").to_string_lossy().to_string())
            .unwrap_or_default();

        let mut project_tools: HashMap<String, Value> = HashMap::new();
        let mut global_tools: HashMap<String, Value> = HashMap::new();

        for (tool_name, versions) in obj {
            let arr = versions.as_array()?;
            for entry in arr {
                let version = entry.get("version")?.as_str()?;
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
        Some(result)
    }
}
