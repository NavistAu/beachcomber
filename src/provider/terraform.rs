use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    Source, SourceMetadata, SourceResult, Value,
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
        invalidation: InvalidationStrategy::Watch {
            patterns: vec![".terraform/environment".into()],
            abs_paths: vec![],
        },
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 60,
        },
        decay: None,
        budget: crate::scheduler::lifecycle::DEFAULT_BUDGET,
        timeout: crate::scheduler::lifecycle::DEFAULT_TIMEOUT,
        nearest: true,
    }
}

struct TerraformState;

impl Source for TerraformState {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(state_source_metadata)
    }

    fn execute(&self, path: Option<&str>) -> Result<SourceResult, String> {
        let Some(path) = path else {
            return Err("no directory to run in".into());
        };
        let dir = Path::new(path);
        let tf_dir = dir.join(".terraform");
        if !tf_dir.exists() {
            return Ok(SourceResult::new());
        }

        // Read from .terraform/environment file only. $TF_WORKSPACE is a
        // per-shell override rendered client-side by the workspace template
        // in defaults/terraform.toml:
        //   workspace = "{{ env.TF_WORKSPACE or comb.terraform.workspace }}"
        let mut result = SourceResult::new();
        if let Some(workspace) = read_workspace_file(&tf_dir) {
            result.insert("workspace", Value::String(workspace));
        }
        Ok(result)
    }

    fn root(&self, dir: &str) -> Result<Option<String>, String> {
        Ok(find_terraform_root(Path::new(dir)))
    }

    fn has_root(&self) -> bool {
        true
    }
}

/// The selected workspace: `.terraform/environment`, or `default` when there
/// is no such file, as terraform reads it. An empty file names none.
fn read_workspace_file(tf_dir: &Path) -> Option<String> {
    let workspace = std::fs::read_to_string(tf_dir.join("environment"))
        .unwrap_or_else(|_| "default".to_string())
        .trim()
        .to_string();
    (!workspace.is_empty()).then_some(workspace)
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

/// Tests for the terraform provider.
/// Uses tempdir to isolate from the real filesystem.
#[cfg(test)]
mod tests {
    use super::TerraformProvider;
    use crate::provider::Provider;

    fn make_tf_root(ws_name: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".terraform")).unwrap();
        if let Some(ws) = ws_name {
            std::fs::write(dir.path().join(".terraform").join("environment"), ws).unwrap();
        }
        dir
    }

    #[test]
    fn terraform_workspace_field_present() {
        // Confirms that the daemon field is named 'workspace'.
        // This is the field the virtual cascade 'workspace = "env.TF_WORKSPACE or comb.terraform.workspace"' references.
        let dir = make_tf_root(Some("staging"));
        let sources = TerraformProvider.sources();
        let result = sources[0]
            .execute(Some(dir.path().to_str().unwrap()))
            .unwrap();
        assert!(
            result.fields.contains_key("workspace"),
            "daemon field must be 'workspace'; got keys: {:?}",
            result.fields.keys().collect::<Vec<_>>()
        );
        assert!(
            !result.fields.contains_key("path_workspace"),
            "'path_workspace' key must be gone; found it in: {:?}",
            result.fields.keys().collect::<Vec<_>>()
        );
        assert_eq!(result.fields["workspace"].as_text(), "staging");
    }

    #[test]
    fn terraform_no_env_read_for_tf_workspace() {
        // Confirms the daemon no longer reads $TF_WORKSPACE.
        let dir = make_tf_root(Some("from-file"));
        let result = temp_env::with_var("TF_WORKSPACE", Some("from-env"), || {
            let sources = TerraformProvider.sources();
            sources[0]
                .execute(Some(dir.path().to_str().unwrap()))
                .unwrap()
        });
        // Daemon must return 'from-file' (file value), not 'from-env' (env).
        assert_eq!(
            result.fields.get("workspace").unwrap().as_text(),
            "from-file",
            "daemon must not read $TF_WORKSPACE; that is the `terraform:template` default"
        );
    }

    #[test]
    fn terraform_no_environment_file_defaults_to_default() {
        let dir = make_tf_root(None); // .terraform exists but no environment file
        let result = temp_env::with_var("TF_WORKSPACE", None::<&str>, || {
            let sources = TerraformProvider.sources();
            sources[0]
                .execute(Some(dir.path().to_str().unwrap()))
                .unwrap()
        });
        assert_eq!(
            result.fields.get("workspace").unwrap().as_text(),
            "default",
            "no environment file → default workspace"
        );
    }

    #[test]
    fn terraform_returns_none_without_terraform_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let sources = TerraformProvider.sources();
        let result = sources[0]
            .execute(Some(tmp.path().to_str().unwrap()))
            .unwrap();
        assert!(result.fields.is_empty());
    }

    #[test]
    fn terraform_root_returns_module_root_from_subdir() {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join(".terraform")).unwrap();
        let subdir = project.path().join("envs").join("dev");
        std::fs::create_dir_all(&subdir).unwrap();

        let sources = TerraformProvider.sources();
        let got = sources[0].root(subdir.to_str().unwrap()).unwrap();
        let expected = project.path().to_string_lossy().to_string();
        assert_eq!(got, Some(expected));
    }

    #[test]
    fn terraform_root_none_outside_project() {
        let tmp = tempfile::tempdir().unwrap();
        let sources = TerraformProvider.sources();
        let got = sources[0].root(tmp.path().to_str().unwrap()).unwrap();
        if let Some(got) = got {
            assert_ne!(got, tmp.path().to_string_lossy().to_string());
        }
    }
}
