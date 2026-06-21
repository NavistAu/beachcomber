use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use std::collections::HashMap;
use std::path::PathBuf;

pub struct KubecontextProvider;

impl Provider for KubecontextProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "kubecontext".into(),
            sources: vec![context_source_metadata()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(KubeContext::new())]
    }
}

fn context_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "context".into(),
        fields: vec![
            FieldSchema {
                name: "context".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "namespace".into(),
                field_type: FieldType::String,
            },
        ],
        // PathScoped: the instance path is the kubeconfig path (single, or a
        // ':'-joined list) computed by the CLI from $KUBECONFIG via the provider's
        // path expression. The daemon never reads $KUBECONFIG. Watch: the scheduler
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

struct KubeContext;

impl KubeContext {
    fn new() -> Self {
        Self
    }
}

impl Source for KubeContext {
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

        let (current_context, ns_map) = merge_kubeconfigs(&files);
        let Some(context) = current_context else {
            return SourceResult::new();
        };

        let namespace = ns_map
            .get(&context)
            .cloned()
            .unwrap_or_else(|| "default".to_string());

        let mut result = SourceResult::new();
        result.insert("context", Value::String(context));
        result.insert("namespace", Value::String(namespace));
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

/// Merge multiple kubeconfig files.
/// Returns (last non-empty current-context, map of context-name → namespace).
/// Later files win on conflict (kubectl merge semantics).
/// Unreadable / non-existent files are silently skipped.
fn merge_kubeconfigs(paths: &[PathBuf]) -> (Option<String>, HashMap<String, String>) {
    let mut current_context: Option<String> = None;
    let mut ns_map: HashMap<String, String> = HashMap::new();

    for path in paths {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        // Extract current-context from this file.
        if let Some(ctx) = content
            .lines()
            .find(|l| l.starts_with("current-context:"))
            .and_then(|l| {
                l.strip_prefix("current-context:")
                    .map(|s| s.trim().to_string())
            })
            .filter(|s| !s.is_empty())
        {
            current_context = Some(ctx);
        }
        // Extract all context name→namespace entries from this file.
        for (name, ns) in extract_context_namespaces(&content) {
            ns_map.insert(name, ns);
        }
    }

    (current_context, ns_map)
}

/// Parse all (context-name, namespace) pairs from a kubeconfig file content.
/// Uses exact name matching (not substring).
fn extract_context_namespaces(content: &str) -> Vec<(String, String)> {
    let mut in_contexts = false;
    let mut current_block = String::new();
    let mut blocks: Vec<String> = Vec::new();

    for line in content.lines() {
        if line.trim().starts_with("contexts:") && !line.starts_with(' ') {
            in_contexts = true;
            continue;
        }
        if in_contexts && !line.starts_with(' ') && !line.starts_with('-') && !line.is_empty() {
            in_contexts = false;
            if !current_block.is_empty() {
                blocks.push(current_block.clone());
                current_block.clear();
            }
            continue;
        }
        if in_contexts {
            if line.starts_with("- ") && !current_block.is_empty() {
                blocks.push(current_block.clone());
                current_block.clear();
            }
            current_block.push_str(line);
            current_block.push('\n');
        }
    }
    if !current_block.is_empty() {
        blocks.push(current_block);
    }

    let mut out = Vec::new();
    for block in &blocks {
        // Extract exact name: must be `  name: <value>` with no trailing chars on same key.
        let name = block.lines().find_map(|line| {
            let trimmed = line.trim();
            // Match lines that are exactly `name: <something>` — key must equal "name".
            if let Some((key, val)) = trimmed.split_once(':')
                && key.trim() == "name"
            {
                let v = val.trim().trim_matches('"').to_string();
                if !v.is_empty() {
                    return Some(v);
                }
            }
            None
        });
        let Some(name) = name else { continue };

        let namespace = block.lines().find_map(|line| {
            let trimmed = line.trim();
            if let Some((key, val)) = trimmed.split_once(':')
                && key.trim() == "namespace"
            {
                let v = val.trim().trim_matches('"').to_string();
                if !v.is_empty() {
                    return Some(v);
                }
            }
            None
        });

        out.push((name, namespace.unwrap_or_else(|| "default".to_string())));
    }
    out
}
