use crate::provider::{
    FieldSchema, FieldScope, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};
use std::path::Path;
use std::process::Command;

pub struct MiseProvider;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MiseFilter {
    Global,
    Project,
}

fn run_mise_and_filter(
    cwd: &str,
    global_config_dir: &str,
    filter: MiseFilter,
) -> Option<std::collections::HashMap<String, Value>> {
    let output = Command::new("mise")
        .args(["ls", "--current", "--json"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|o| o.status.success())?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).ok()?;
    let obj = parsed.as_object()?;

    let mut tools: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    for (tool_name, versions) in obj {
        let Some(arr) = versions.as_array() else {
            continue;
        };
        for entry in arr {
            let Some(version) = entry.get("version").and_then(|v| v.as_str()) else {
                continue;
            };
            let source_path = entry
                .get("source")
                .and_then(|s| s.get("path"))
                .and_then(|p| p.as_str())
                .unwrap_or("");
            let is_global = source_path.starts_with(global_config_dir);
            let include = match filter {
                MiseFilter::Global => is_global,
                MiseFilter::Project => !is_global,
            };
            if include {
                tools.insert(tool_name.clone(), Value::String(version.to_string()));
            }
        }
    }
    Some(tools)
}

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
                    scope: FieldScope::Global,
                },
            ],
            invalidation: InvalidationStrategy::Watch {
                patterns: vec![".mise.toml".to_string(), "mise.toml".to_string()],
                fallback_poll_secs: Some(30),
            },
        }
    }

    fn execute(&self, path: Option<&str>) -> Vec<(Option<String>, ProviderResult)> {
        let mut out = Vec::new();

        let global_config_dir = std::env::var("XDG_CONFIG_HOME")
            .or_else(|_| std::env::var("HOME").map(|h| format!("{h}/.config")))
            .map(|c| Path::new(&c).join("mise").to_string_lossy().to_string())
            .unwrap_or_default();

        // Global entry — always emit, compute from $HOME so no project config influences it.
        if let Ok(home) = std::env::var("HOME")
            && let Some(global_tools) =
                run_mise_and_filter(&home, &global_config_dir, MiseFilter::Global)
        {
            let mut result = ProviderResult::new();
            result.insert("global", Value::Object(global_tools));
            out.push((None, result));
        }

        // Project entry — only if path has a local mise config.
        if let Some(p) = path {
            let dir = Path::new(p);
            let has_config = dir.join("mise.toml").exists() || dir.join(".mise.toml").exists();
            if has_config
                && let Some(project_tools) =
                    run_mise_and_filter(p, &global_config_dir, MiseFilter::Project)
            {
                let mut result = ProviderResult::new();
                result.insert("project", Value::Object(project_tools));
                out.push((Some(p.to_string()), result));
            }
        }

        out
    }
}
