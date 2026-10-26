use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    Source, SourceMetadata, SourceResult, Value,
};
use std::path::Path;

pub struct PythonProvider;

impl Provider for PythonProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "python".into(),
            sources: vec![venv_source_metadata()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(PythonVenv)]
    }
}

fn venv_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "venv".into(),
        fields: vec![
            FieldSchema {
                name: "venv".into(),
                field_type: FieldType::Bool,
            },
            FieldSchema {
                name: "local_venv_name".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "venv_version".into(),
                field_type: FieldType::String,
            },
        ],
        invalidation: InvalidationStrategy::Watch {
            patterns: vec![".venv".into(), "pyproject.toml".into()],
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

struct PythonVenv;

impl Source for PythonVenv {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(venv_source_metadata)
    }

    fn execute(&self, path: Option<&str>) -> Result<SourceResult, String> {
        let Some(path) = path else {
            return Err("no directory to run in".into());
        };
        let dir = Path::new(path);

        // Scan common venv directory names. $VIRTUAL_ENV is a per-shell var
        // rendered client-side by the venv_name template in defaults/python.toml:
        //   venv_name = "{{ comb.python.local_venv_name or (env.VIRTUAL_ENV | basename) }}"
        // The daemon only reads from the filesystem at the given path.
        for name in VENV_DIRS {
            let venv_path = dir.join(name);
            let cfg_path = venv_path.join("pyvenv.cfg");
            if cfg_path.exists() {
                let mut result = SourceResult::new();
                result.insert("venv", Value::Bool(true));
                result.insert("local_venv_name", Value::String(name.to_string()));
                if let Some(version) = parse_pyvenv_version(&cfg_path) {
                    result.insert("venv_version", Value::String(version));
                }
                return Ok(result);
            }
        }

        Ok(SourceResult::new())
    }

    fn root(&self, dir: &str) -> Result<Option<String>, String> {
        Ok(find_python_project_root(Path::new(dir)))
    }

    fn has_root(&self) -> bool {
        true
    }
}

/// The `version` a `pyvenv.cfg` records, or `None` when it records none.
fn parse_pyvenv_version(cfg_path: &Path) -> Option<String> {
    let cfg = std::fs::read_to_string(cfg_path).ok()?;
    cfg.lines().find_map(|line| {
        let (key, val) = line.split_once('=')?;
        let v = val.trim();
        (key.trim() == "version" && !v.is_empty()).then(|| v.to_string())
    })
}

const VENV_DIRS: &[&str] = &[".venv", "venv", ".virtualenv", "env"];

fn looks_like_python_project(dir: &Path) -> bool {
    if dir.join("pyproject.toml").exists() {
        return true;
    }
    for name in VENV_DIRS {
        if dir.join(name).join("pyvenv.cfg").exists() {
            return true;
        }
    }
    false
}

fn find_python_project_root(start: &Path) -> Option<String> {
    let mut cur: Option<&Path> = Some(start);
    while let Some(dir) = cur {
        if looks_like_python_project(dir) {
            return Some(dir.to_string_lossy().to_string());
        }
        cur = dir.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::PythonProvider;
    use crate::provider::Provider;
    use tempfile::TempDir;

    fn make_venv(name: &str, version: &str) -> TempDir {
        let dir = TempDir::new().unwrap();
        let venv = dir.path().join(name);
        std::fs::create_dir_all(venv.join("bin")).unwrap();
        let cfg = format!("version = {version}\nhome = /usr/bin\n");
        std::fs::write(venv.join("pyvenv.cfg"), cfg).unwrap();
        dir
    }

    #[test]
    fn python_field_is_venv_version_not_version() {
        // Confirms the daemon field is 'venv_version', not 'version'.
        let dir = make_venv(".venv", "3.12.1");
        let sources = PythonProvider.sources();
        let result = sources[0]
            .execute(Some(dir.path().to_str().unwrap()))
            .unwrap();
        assert!(
            result.fields.contains_key("venv_version"),
            "field must be 'venv_version'; got: {:?}",
            result.fields.keys().collect::<Vec<_>>()
        );
        assert!(
            !result.fields.contains_key("version"),
            "old 'version' key must be absent"
        );
        assert_eq!(result.fields["venv_version"].as_text(), "3.12.1");
    }

    #[test]
    fn python_local_venv_name_field_present() {
        // The new 'local_venv_name' field holds the raw directory name (.venv, venv, etc.).
        let dir = make_venv(".venv", "3.11.0");
        let sources = PythonProvider.sources();
        let result = sources[0]
            .execute(Some(dir.path().to_str().unwrap()))
            .unwrap();
        assert_eq!(result.fields["local_venv_name"].as_text(), ".venv");
    }

    #[test]
    fn python_no_virtual_env_fallback() {
        // Confirms the daemon ignores $VIRTUAL_ENV.
        let dir = TempDir::new().unwrap(); // no venv on disk
        let result = temp_env::with_var("VIRTUAL_ENV", Some("/some/external/venv"), || {
            let sources = PythonProvider.sources();
            sources[0]
                .execute(Some(dir.path().to_str().unwrap()))
                .unwrap()
        });
        assert!(
            result.fields.is_empty(),
            "daemon must not fall back to $VIRTUAL_ENV; got: {:?}",
            result.fields
        );
    }

    #[test]
    fn python_venv_bool_true_when_venv_found() {
        let dir = make_venv("venv", "3.10.0");
        let sources = PythonProvider.sources();
        let result = sources[0]
            .execute(Some(dir.path().to_str().unwrap()))
            .unwrap();
        assert_eq!(
            result.fields["venv"].as_text(),
            "true",
            "venv field must be bool true"
        );
    }

    #[test]
    fn python_venv_version_exact_key_match_no_version_info_bleed() {
        // virtualenv writes `version_info = X.Y.Z.final.0` and NO bare `version` line.
        // strip_prefix("version") incorrectly matches version_info; exact-key match must not.
        // With no bare `version` key present, venv_version is absent.
        let dir = TempDir::new().unwrap();
        let venv = dir.path().join(".venv");
        std::fs::create_dir_all(venv.join("bin")).unwrap();
        std::fs::write(
            venv.join("pyvenv.cfg"),
            "version_info = 3.12.1.final.0\nhome = /usr/bin\n",
        )
        .unwrap();

        let sources = PythonProvider.sources();
        let result = sources[0]
            .execute(Some(dir.path().to_str().unwrap()))
            .unwrap();
        assert!(
            !result.fields.contains_key("venv_version"),
            "with only version_info present (no bare version key), venv_version is absent: {:?}",
            result.fields
        );
    }

    #[test]
    fn python_venv_version_bare_version_key_returns_value() {
        // Confirm the stdlib venv `version = X.Y.Z` line is still parsed correctly.
        let dir = make_venv(".venv", "3.12.1");
        let sources = PythonProvider.sources();
        let result = sources[0]
            .execute(Some(dir.path().to_str().unwrap()))
            .unwrap();
        assert_eq!(result.fields["venv_version"].as_text(), "3.12.1");
    }
}
