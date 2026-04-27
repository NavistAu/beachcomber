use crate::provider::{
    expand_abs_path, FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive,
    Provider, ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

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

fn global_config_dir() -> String {
    std::env::var("XDG_CONFIG_HOME")
        .or_else(|_| std::env::var("HOME").map(|h| format!("{h}/.config")))
        .map(|c| Path::new(&c).join("mise").to_string_lossy().to_string())
        .unwrap_or_default()
}

// ── SourceMetadata constructors ───────────────────────────────────────────────

fn global_meta() -> SourceMetadata {
    let abs_paths = expand_abs_path("$XDG_CONFIG_HOME/mise")
        .map(|p| vec![p.to_string_lossy().to_string()])
        .unwrap_or_default();
    SourceMetadata {
        name: "global".into(),
        fields: vec![FieldSchema {
            name: "<tool>".into(),
            field_type: FieldType::String,
        }],
        scope: SourceScope::Global,
        invalidation: InvalidationStrategy::Watch {
            patterns: vec![],
            abs_paths,
        },
        keep_alive: KeepAlive::Never,
        failback: FailbackConfig { reattempts: 3, interval_secs: 60 },
        fsevents_reinstate: true,
    }
}

fn project_meta() -> SourceMetadata {
    SourceMetadata {
        name: "project".into(),
        fields: vec![FieldSchema {
            name: "<tool>".into(),
            field_type: FieldType::String,
        }],
        scope: SourceScope::PathScoped,
        invalidation: InvalidationStrategy::Watch {
            patterns: vec![".mise.toml".into(), "mise.toml".into()],
            abs_paths: vec![],
        },
        keep_alive: KeepAlive::Duration(120),
        failback: FailbackConfig { reattempts: 3, interval_secs: 60 },
        fsevents_reinstate: true,
    }
}

// ── Source impls ──────────────────────────────────────────────────────────────

struct MiseGlobal;

impl Source for MiseGlobal {
    fn metadata(&self) -> &SourceMetadata {
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(global_meta)
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        let gcd = global_config_dir();
        let Ok(home) = std::env::var("HOME") else {
            return SourceResult::new();
        };
        let Some(tools) = run_mise_and_filter(&home, &gcd, MiseFilter::Global) else {
            return SourceResult::new();
        };
        let mut result = SourceResult::new();
        for (tool, version) in tools {
            result.insert(tool, version);
        }
        result
    }

    // Global source: no canonical_path needed (uses default identity).
}

struct MiseProject;

impl Source for MiseProject {
    fn metadata(&self) -> &SourceMetadata {
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(project_meta)
    }

    fn execute(&self, path: Option<&str>) -> SourceResult {
        let Some(p) = path else {
            return SourceResult::new();
        };
        let dir = Path::new(p);
        let gcd = global_config_dir();

        let has_config = dir.join("mise.toml").exists() || dir.join(".mise.toml").exists();
        if !has_config {
            return SourceResult::new();
        }

        let Some(tools) = run_mise_and_filter(p, &gcd, MiseFilter::Project) else {
            return SourceResult::new();
        };

        let mut result = SourceResult::new();
        for (tool, version) in tools {
            result.insert(tool, version);
        }
        result
    }

    fn canonical_path(&self, path: Option<&str>) -> Option<String> {
        let p = path?;
        find_mise_project_root(Path::new(p))
    }
}

// ── Provider ──────────────────────────────────────────────────────────────────

impl Provider for MiseProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "mise".into(),
            sources: vec![global_meta(), project_meta()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(MiseGlobal), Box::new(MiseProject)]
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
