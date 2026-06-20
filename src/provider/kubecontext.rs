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
        scope: SourceScope::Global,
        invalidation: InvalidationStrategy::Poll { interval_secs: 30 },
        keep_alive: KeepAlive::Polls(2),
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 30,
        },
        fsevents_reinstate: false,
    }
}

struct KubeContext {
    /// Explicit kubeconfig paths override (test seam). When `None`, the daemon
    /// reads only the default `~/.kube/config` — it does not consult `$KUBECONFIG`
    /// (a per-shell selector; deferred to the P2 `live.*` path).
    override_paths: Option<Vec<PathBuf>>,
}

impl KubeContext {
    fn new() -> Self {
        Self {
            override_paths: None,
        }
    }

    /// Construct a `KubeContext` that reads from a single explicit path,
    /// bypassing the default `~/.kube/config`. Intended for tests.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn with_kubeconfig_path(path: PathBuf) -> Self {
        Self {
            override_paths: Some(vec![path]),
        }
    }

    /// Construct a `KubeContext` that merges multiple explicit paths.
    /// Intended for tests of multi-file merge logic.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn with_kubeconfig_paths(paths: Vec<PathBuf>) -> Self {
        Self {
            override_paths: Some(paths),
        }
    }
}

impl Source for KubeContext {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(context_source_metadata)
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        let paths = if let Some(ref p) = self.override_paths {
            p.clone()
        } else {
            kubeconfig_paths()
        };

        if paths.is_empty() {
            return SourceResult::new();
        }

        let (current_context, ns_map) = merge_kubeconfigs(&paths);
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
}

/// Return the kubeconfig path the daemon reads.
///
/// The daemon is path-only (P1 env-cascade design): it reads the default
/// `~/.kube/config` and deliberately does NOT consult `$KUBECONFIG`.
/// `$KUBECONFIG` is a per-shell selector — it chooses which cluster/context is
/// active and which files are merged — so a single daemon-frozen value would be
/// wrong for every other shell. Honoring a caller's `$KUBECONFIG` (a per-shell
/// override) is deferred to the P2 `live.*` path. Only `$HOME` is consulted, as
/// a file-location var (the same way other providers locate their config files).
fn kubeconfig_paths() -> Vec<PathBuf> {
    let Ok(home) = std::env::var("HOME") else {
        return vec![];
    };
    vec![PathBuf::from(home).join(".kube").join("config")]
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

/// Construct the kubecontext source reading from an explicit single kubeconfig path.
/// Intended for seam tests — bypasses the default `~/.kube/config`.
#[cfg(any(test, feature = "test-helpers"))]
pub fn kubecontext_source_with_path(path: PathBuf) -> Box<dyn Source> {
    Box::new(KubeContext::with_kubeconfig_path(path))
}

/// Construct the kubecontext source reading from multiple explicit kubeconfig paths.
/// Intended for seam tests of multi-file merge logic.
#[cfg(any(test, feature = "test-helpers"))]
pub fn kubecontext_source_with_paths(paths: Vec<PathBuf>) -> Box<dyn Source> {
    Box::new(KubeContext::with_kubeconfig_paths(paths))
}
