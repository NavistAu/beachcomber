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
            name: "workspace".into(),
            field_type: FieldType::String,
        }],
        scope: SourceScope::PathScoped,
        // Narrowed from ".terraform" dir to ".terraform/environment" file to prevent
        // init/lock churn from causing spurious invalidations.
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

        // $TF_WORKSPACE has highest precedence. Note: this reads the daemon's env,
        // not the querying shell's — for per-shell correctness see S5 (selector protocol).
        // For headless/CI use and system-set vars this is already correct.
        let workspace = if let Ok(ws) = std::env::var("TF_WORKSPACE") {
            let ws = ws.trim().to_string();
            if !ws.is_empty() {
                ws
            } else {
                read_workspace_file(&tf_dir)
            }
        } else {
            read_workspace_file(&tf_dir)
        };

        // LIMITATION: remote/cloud backends without a local .terraform/environment file
        // return "default" here. Determining the actual remote workspace would require
        // a network call (out of scope for S6).

        let mut result = SourceResult::new();
        result.insert("workspace", Value::String(workspace));
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
