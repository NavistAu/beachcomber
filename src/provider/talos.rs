use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use std::path::PathBuf;

pub struct TalosProvider;

impl Provider for TalosProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "talos".into(),
            sources: vec![context_source_metadata()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(TalosContext)]
    }
}

fn context_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "context".into(),
        fields: vec![FieldSchema {
            name: "context".into(),
            field_type: FieldType::String,
        }],
        // PathScoped: the instance path is the talosconfig path (single, or a
        // ':'-joined list) computed by the CLI from $TALOSCONFIG via the provider's
        // path expression. The daemon never reads $TALOSCONFIG. Watch: the scheduler
        // watches the path's component files via Source::watched_files.
        scope: SourceScope::PathScoped,
        invalidation: InvalidationStrategy::Watch {
            patterns: vec![],
            abs_paths: vec![],
        },
        keep_alive: KeepAlive::Duration(120),
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 30,
        },
        fsevents_reinstate: true,
    }
}

fn path_to_files(path: &str) -> Vec<PathBuf> {
    path.split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

struct TalosContext;

impl Source for TalosContext {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(context_source_metadata)
    }

    fn execute(&self, path: Option<&str>) -> SourceResult {
        let Some(p) = path else {
            return SourceResult::new();
        };
        let files = path_to_files(p);
        if files.is_empty() {
            return SourceResult::new();
        }

        let mut context: Option<String> = None;
        for f in &files {
            if let Ok(c) = std::fs::read_to_string(f)
                && let Some(n) = parse_active_context(&c)
            {
                context = Some(n);
            }
        }

        let Some(context) = context else {
            return SourceResult::new();
        };

        let mut result = SourceResult::new();
        result.insert("context", Value::String(context));
        result
    }

    fn canonical_path(&self, path: Option<&str>) -> Option<String> {
        let p = path?;
        let joined = p
            .split(':')
            .filter(|s| !s.is_empty())
            .map(|c| {
                std::fs::canonicalize(c)
                    .map(|q| q.to_string_lossy().to_string())
                    .unwrap_or_else(|_| c.to_string())
            })
            .collect::<Vec<_>>()
            .join(":");
        if joined.is_empty() {
            None
        } else {
            Some(joined)
        }
    }

    fn watched_files(&self, path: Option<&str>) -> Vec<PathBuf> {
        path.map(path_to_files).unwrap_or_default()
    }
}

/// Parse the top-level `context: <name>` key from a talosconfig YAML file.
/// Only top-level keys (no leading whitespace) are considered.
fn parse_active_context(content: &str) -> Option<String> {
    for line in content.lines() {
        // Only consider top-level keys (no leading whitespace).
        if line.starts_with(|c: char| c.is_whitespace()) {
            continue;
        }
        if let Some((k, v)) = line.split_once(':')
            && k.trim() == "context"
        {
            let v = v.trim().trim_matches('"').to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}
