use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use std::path::Path;

pub struct TerraformProvider;

impl Provider for TerraformProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "terraform".into(),
            sources: vec![state_source_metadata()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(TerraformState)]
    }
}

fn state_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "state".into(),
        fields: vec![FieldSchema {
            name: "path_workspace".into(),
            field_type: FieldType::String,
        }],
        scope: SourceScope::PathScoped,
        invalidation: InvalidationStrategy::Watch {
            patterns: vec![".terraform/environment".into()],
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

struct TerraformState;

impl Source for TerraformState {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(state_source_metadata)
    }

    fn execute(&self, path: Option<&str>) -> SourceResult {
        let Some(path) = path else {
            return SourceResult::new();
        };
        let dir = Path::new(path);
        let tf_dir = dir.join(".terraform");
        if !tf_dir.exists() {
            return SourceResult::new();
        }

        // Read from .terraform/environment file only. $TF_WORKSPACE is a
        // per-shell override resolved client-side in the virtual cascade:
        //   workspace = "env.TF_WORKSPACE or terraform.path_workspace"
        let workspace = read_workspace_file(&tf_dir);

        let mut result = SourceResult::new();
        result.insert("path_workspace", Value::String(workspace));
        result
    }

    fn canonical_path(&self, path: Option<&str>) -> Option<String> {
        let p = path?;
        find_terraform_root(Path::new(p))
    }
}

fn read_workspace_file(tf_dir: &Path) -> String {
    std::fs::read_to_string(tf_dir.join("environment"))
        .unwrap_or_else(|_| "default".to_string())
        .trim()
        .to_string()
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
