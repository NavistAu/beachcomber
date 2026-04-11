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
            fields: vec![
                FieldSchema {
                    name: "tools".to_string(),
                    field_type: FieldType::String,
                },
                FieldSchema {
                    name: "project".to_string(),
                    field_type: FieldType::String,
                },
                FieldSchema {
                    name: "global".to_string(),
                    field_type: FieldType::String,
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

        let mut project_tools = Vec::new();
        let mut global_tools = Vec::new();

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

                let pair = format!("{tool_name}={version}");
                if is_global {
                    global_tools.push(pair);
                } else {
                    project_tools.push(pair);
                }
            }
        }

        project_tools.sort();
        global_tools.sort();

        // Combined view: project tools with P: prefix, global with G:
        let mut combined = Vec::new();
        for t in &project_tools {
            combined.push(format!("P:{t}"));
        }
        for t in &global_tools {
            combined.push(format!("G:{t}"));
        }

        let mut result = ProviderResult::new();
        result.insert("tools", Value::String(combined.join(",")));
        result.insert("project", Value::String(project_tools.join(",")));
        result.insert("global", Value::String(global_tools.join(",")));
        Some(result)
    }
}
