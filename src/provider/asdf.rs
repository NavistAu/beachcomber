use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use std::path::Path;

pub struct AsdfProvider;

impl Provider for AsdfProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "asdf".into(),
            sources: vec![tools_source_metadata()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(AsdfTools)]
    }
}

fn tools_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "tools".into(),
        fields: vec![FieldSchema {
            name: "<tool>".into(),
            field_type: FieldType::String,
        }],
        scope: SourceScope::PathScoped,
        invalidation: InvalidationStrategy::Watch {
            patterns: vec![".tool-versions".into()],
            abs_paths: vec![],
        },
        keep_alive: KeepAlive::Duration(120),
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 60,
        },
        fsevents_reinstate: true,
    }
}

struct AsdfTools;

impl Source for AsdfTools {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(tools_source_metadata)
    }

    fn execute(&self, path: Option<&str>) -> SourceResult {
        let Some(path) = path else {
            return SourceResult::new();
        };
        let dir = Path::new(path);

        // Try to find a .tool-versions file: first walk from dir, then global fallback.
        let Some(content) = find_tool_versions_content(dir) else {
            return SourceResult::new();
        };

        let mut result = SourceResult::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                // Emit primary version as flat field.
                // NOTE: multi-version fallback lists (e.g. "node 20.11.0 18.19.0") are
                // documented as a known limitation — only the primary version is emitted.
                result.insert(parts[0], Value::String(parts[1].to_string()));
            }
        }
        result
    }

    fn canonical_path(&self, path: Option<&str>) -> Option<String> {
        let p = path?;
        find_tool_versions_root_with_global(Path::new(p))
    }
}

/// Walk up from start looking for .tool-versions. Return the directory containing it,
/// or None if not found (local walk only — no global fallback here, used for cache key).
fn find_tool_versions_root_with_global(start: &Path) -> Option<String> {
    let mut cur: Option<&Path> = Some(start);
    while let Some(dir) = cur {
        if dir.join(".tool-versions").exists() {
            return Some(dir.to_string_lossy().to_string());
        }
        cur = dir.parent();
    }
    // Global fallback: use HOME as the cache key
    if let Ok(home) = std::env::var("HOME") {
        let global = Path::new(&home).join(".tool-versions");
        if global.exists() {
            return Some(home);
        }
        let xdg_global = Path::new(&home)
            .join(".config")
            .join("asdf")
            .join("tool-versions");
        if xdg_global.exists() {
            return Some(home);
        }
    }
    None
}

/// Read the content of the first .tool-versions found, walking up from start.
/// Falls back to ~/.tool-versions and ~/.config/asdf/tool-versions if no local file found.
fn find_tool_versions_content(start: &Path) -> Option<String> {
    // Walk up from start.
    let mut cur: Option<&Path> = Some(start);
    while let Some(dir) = cur {
        let candidate = dir.join(".tool-versions");
        if candidate.exists() {
            return std::fs::read_to_string(&candidate).ok();
        }
        cur = dir.parent();
    }
    // Global fallback: ~/.tool-versions
    if let Ok(home) = std::env::var("HOME") {
        let global = Path::new(&home).join(".tool-versions");
        if global.exists()
            && let Ok(c) = std::fs::read_to_string(&global)
        {
            return Some(c);
        }
        // XDG alternative: ~/.config/asdf/tool-versions
        let xdg_global = Path::new(&home)
            .join(".config")
            .join("asdf")
            .join("tool-versions");
        if xdg_global.exists() {
            return std::fs::read_to_string(&xdg_global).ok();
        }
    }
    None
}
