use crate::provider::{
    FieldSchema, FieldScope, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};
use std::path::Path;

pub struct PythonProvider;

impl Provider for PythonProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "python".to_string(),
            fields: vec![
                FieldSchema {
                    name: "venv".to_string(),
                    field_type: FieldType::Bool,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "venv_name".to_string(),
                    field_type: FieldType::String,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "version".to_string(),
                    field_type: FieldType::String,
                    scope: FieldScope::PathScoped,
                },
            ],
            invalidation: InvalidationStrategy::Watch {
                patterns: vec![".venv".to_string(), "pyproject.toml".to_string()],
                fallback_poll_secs: Some(30),
            },
            global: false,
        }
    }

    fn execute(&self, path: Option<&str>) -> Option<ProviderResult> {
        let path = path?;
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
        if !venv_found && let Ok(venv_path) = std::env::var("VIRTUAL_ENV") {
            let p = Path::new(&venv_path);
            if p.exists() {
                venv_found = true;
                venv_name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
            }
        }

        if !venv_found {
            return None;
        }

        let mut result = ProviderResult::new();
        result.insert("venv", Value::Bool(true));
        result.insert("venv_name", Value::String(venv_name));
        result.insert("version", Value::String(version));
        Some(result)
    }
}
