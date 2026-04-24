use crate::provider::{
    FieldSchema, FieldScope, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};
use std::path::Path;

pub struct TerraformProvider;

impl Provider for TerraformProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "terraform".to_string(),
            fields: vec![FieldSchema {
                name: "workspace".to_string(),
                field_type: FieldType::String,
                scope: FieldScope::PathScoped,
            }],
            invalidation: InvalidationStrategy::Watch {
                patterns: vec![".terraform".to_string()],
                fallback_poll_secs: Some(30),
            },
        }
    }

    // Walk up from `path` to the nearest directory containing `.terraform`.
    // Terraform operations are executed relative to the module root (where
    // terraform init writes `.terraform`), so subdirs should share that cache
    // entry.
    fn canonical_path(&self, path: Option<&str>) -> Option<String> {
        let p = path?;
        find_terraform_root(Path::new(p))
    }

    fn execute(&self, path: Option<&str>) -> Vec<(Option<String>, ProviderResult)> {
        let Some(path) = path else {
            return Vec::new();
        };
        let path_owned = path.to_string();
        let dir = Path::new(path);
        let tf_dir = dir.join(".terraform");
        if !tf_dir.exists() {
            return Vec::new();
        }

        // Read workspace from .terraform/environment
        let workspace = std::fs::read_to_string(tf_dir.join("environment"))
            .unwrap_or_else(|_| "default".to_string())
            .trim()
            .to_string();

        let mut result = ProviderResult::new();
        result.insert("workspace", Value::String(workspace));
        vec![(Some(path_owned), result)]
    }
}

fn find_terraform_root(start: &Path) -> Option<String> {
    let mut cur: Option<&Path> = Some(start);
    while let Some(dir) = cur {
        if dir.join(".terraform").exists() {
            return Some(dir.to_string_lossy().to_string());
        }
        cur = dir.parent();
    }
    None
}
