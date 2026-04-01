use crate::provider::{
    FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata, ProviderResult, Value,
};

pub struct KubecontextProvider;

impl Provider for KubecontextProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "kubecontext".to_string(),
            fields: vec![
                FieldSchema {
                    name: "context".to_string(),
                    field_type: FieldType::String,
                },
                FieldSchema {
                    name: "namespace".to_string(),
                    field_type: FieldType::String,
                },
            ],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 30,
                floor_secs: 5,
            },
            global: true,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Option<ProviderResult> {
        let config_path = kubeconfig_path()?;
        let content = std::fs::read_to_string(&config_path).ok()?;

        // Find current-context
        let context = content
            .lines()
            .find(|l| l.starts_with("current-context:"))
            .map(|l| {
                l.strip_prefix("current-context:")
                    .unwrap_or("")
                    .trim()
                    .to_string()
            })
            .filter(|s| !s.is_empty())?;

        // Find namespace for this context
        let namespace =
            find_context_namespace(&content, &context).unwrap_or_else(|| "default".to_string());

        let mut result = ProviderResult::new();
        result.insert("context", Value::String(context));
        result.insert("namespace", Value::String(namespace));
        Some(result)
    }
}

fn kubeconfig_path() -> Option<std::path::PathBuf> {
    if let Ok(path) = std::env::var("KUBECONFIG") {
        // KUBECONFIG can be colon-separated, take the first
        let first = path.split(':').next()?;
        return Some(std::path::PathBuf::from(first));
    }
    let home = std::env::var("HOME").ok()?;
    Some(std::path::PathBuf::from(home).join(".kube").join("config"))
}

fn find_context_namespace(content: &str, context_name: &str) -> Option<String> {
    // Split the contexts section into per-item blocks and search for the matching name.
    // Kubeconfig YAML structure:
    //   contexts:
    //   - context:
    //       namespace: my-ns
    //     name: my-context
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

    // Find the block containing our context name
    for block in &blocks {
        if block.contains(&format!("name: {context_name}"))
            || block.contains(&format!("name: \"{context_name}\""))
        {
            // Look for namespace in this block
            for line in block.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("namespace:") {
                    let ns = trimmed
                        .strip_prefix("namespace:")
                        .unwrap_or("")
                        .trim()
                        .trim_matches('"')
                        .to_string();
                    if !ns.is_empty() {
                        return Some(ns);
                    }
                }
            }
        }
    }
    None
}
