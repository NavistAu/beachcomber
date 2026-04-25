use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use std::collections::HashMap;
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
        let tool_versions = dir.join(".tool-versions");

        if !tool_versions.exists() {
            return SourceResult::new();
        }

        let Some(content) = std::fs::read_to_string(&tool_versions).ok() else {
            return SourceResult::new();
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

        let mut result = SourceResult::new();
        result.insert("tools", Value::Object(tools));
        result
    }

    fn canonical_path(&self, path: Option<&str>) -> Option<String> {
        let p = path?;
        find_tool_versions_root(Path::new(p))
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
