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
            // Fields are dynamic: one String field per tool name (e.g. "node", "rust").
            // The sentinel "<tool>" drives inferred_scope=PathScoped so queries with a
            // CWD route to the path-scoped project entry; queries without any path
            // context fall through to the pathless global entry.
            fields: vec![FieldSchema {
                name: "<tool>".to_string(),
                field_type: FieldType::String,
                scope: FieldScope::PathScoped,
            }],
            invalidation: InvalidationStrategy::Watch {
                patterns: vec![".mise.toml".to_string(), "mise.toml".to_string()],
                fallback_poll_secs: Some(30),
            },
        }
    }

    // Mise config changes are rare; keep project entries warm across shell idle
    // so a subsequent mise.toml edit takes effect immediately rather than
    // racing the decay→eviction window. Users can override with
    // `[providers.mise] fsevents_reinstate = false`.
    fn fsevents_reinstate_default(&self) -> bool {
        true
    }

    // Walk up from `path` to find the nearest directory containing
    // `mise.toml` or `.mise.toml`. Returns that directory so shells in
    // subdirectories of a mise project share a single cache entry.
    //
    // Returns None if no mise config is found upward. With the default impl
    // (returning Some(path) regardless) mise project lookups from subdirs
    // silently returned empty results because `execute` checks for marker
    // files directly in `path`. Now the scheduler declines demand instead,
    // which is cleaner and matches git's behaviour.
    fn canonical_path(&self, path: Option<&str>) -> Option<String> {
        let p = path?;
        find_mise_project_root(Path::new(p))
    }

    fn execute(&self, path: Option<&str>) -> Vec<(Option<String>, ProviderResult)> {
        let mut out = Vec::new();

        let global_config_dir = std::env::var("XDG_CONFIG_HOME")
            .or_else(|_| std::env::var("HOME").map(|h| format!("{h}/.config")))
            .map(|c| Path::new(&c).join("mise").to_string_lossy().to_string())
            .unwrap_or_default();

        match path {
            None => {
                // Global entry: called with no path, emit only global tools (pathless slot).
                // Each tool becomes its own field: e.g. result["node"] = "20.1.0".
                if let Ok(home) = std::env::var("HOME")
                    && let Some(global_tools) =
                        run_mise_and_filter(&home, &global_config_dir, MiseFilter::Global)
                {
                    let mut result = ProviderResult::new();
                    for (tool, version) in global_tools {
                        result.insert(tool, version);
                    }
                    out.push((None, result));
                }
            }
            Some(p) => {
                // Project entry: emit only project-scoped tools for the given path.
                // Does not cross-emit global data — global has its own lifecycle entry
                // when demanded via comb get mise (no path context).
                let dir = Path::new(p);
                let has_config =
                    dir.join("mise.toml").exists() || dir.join(".mise.toml").exists();
                if has_config
                    && let Some(project_tools) =
                        run_mise_and_filter(p, &global_config_dir, MiseFilter::Project)
                {
                    let mut result = ProviderResult::new();
                    for (tool, version) in project_tools {
                        result.insert(tool, version);
                    }
                    out.push((Some(p.to_string()), result));
                }
            }
        }

        out
    }
}

/// Walk upwards from `start` looking for a directory that contains either
/// `mise.toml` or `.mise.toml`. Returns the containing directory's path,
/// or `None` if no mise config is found before reaching the filesystem root.
fn find_mise_project_root(start: &Path) -> Option<String> {
    let mut cur: Option<&Path> = Some(start);
    while let Some(dir) = cur {
        if dir.join("mise.toml").exists() || dir.join(".mise.toml").exists() {
            return Some(dir.to_string_lossy().to_string());
        }
        cur = dir.parent();
    }
    None
}
