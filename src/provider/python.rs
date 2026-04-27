use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
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
                name: "venv_name".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "version".into(),
                field_type: FieldType::String,
            },
        ],
        scope: SourceScope::PathScoped,
        invalidation: InvalidationStrategy::Watch {
            patterns: vec![".venv".into(), "pyproject.toml".into()],
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

struct PythonVenv;

impl Source for PythonVenv {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(venv_source_metadata)
    }

    fn execute(&self, path: Option<&str>) -> SourceResult {
        let Some(path) = path else {
            return SourceResult::new();
        };
        let dir = Path::new(path);

        // Check for common venv directory names
        let venv_dirs = [".venv", "venv", ".virtualenv", "env"];
        let mut venv_found = false;
        let mut venv_name = String::new();
        let mut version = String::new();

        for name in &venv_dirs {
            let venv_path = dir.join(name);
            let cfg_path = venv_path.join("pyvenv.cfg");
            if cfg_path.exists() {
                venv_found = true;
                venv_name = name.to_string();
                // Parse version from pyvenv.cfg
                if let Ok(cfg) = std::fs::read_to_string(&cfg_path) {
                    for line in cfg.lines() {
                        if let Some(v) = line.strip_prefix("version") {
                            let v = v.trim_start_matches([' ', '=']).trim();
                            version = v.to_string();
                            break;
                        }
                    }
                }
                break;
            }
        }

        // Also check VIRTUAL_ENV env var
        if !venv_found {
            if let Ok(venv_path) = std::env::var("VIRTUAL_ENV") {
                let p = Path::new(&venv_path);
                if p.exists() {
                    venv_found = true;
                    venv_name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                }
            }
        }

        if !venv_found {
            return SourceResult::new();
        }

        let mut result = SourceResult::new();
        result.insert("venv", Value::Bool(true));
        result.insert("venv_name", Value::String(venv_name));
        result.insert("version", Value::String(version));
        result
    }

    fn canonical_path(&self, path: Option<&str>) -> Option<String> {
        let p = path?;
        find_python_project_root(Path::new(p))
    }
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
