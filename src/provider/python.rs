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
                name: "local_venv_name".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "venv_version".into(),
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

        // Scan common venv directory names. $VIRTUAL_ENV is a per-shell var
        // resolved client-side via:
        //   venv_name = "python.local_venv_name or (env.VIRTUAL_ENV | basename)"
        // The daemon only reads from the filesystem at the given path.
        for name in VENV_DIRS {
            let venv_path = dir.join(name);
            let cfg_path = venv_path.join("pyvenv.cfg");
            if cfg_path.exists() {
                let venv_version = parse_pyvenv_version(&cfg_path);
                let mut result = SourceResult::new();
                result.insert("venv", Value::Bool(true));
                result.insert("local_venv_name", Value::String(name.to_string()));
                result.insert("venv_version", Value::String(venv_version));
                return result;
            }
        }

        SourceResult::new()
    }

    fn canonical_path(&self, path: Option<&str>) -> Option<String> {
        let p = path?;
        find_python_project_root(Path::new(p))
    }
}

fn parse_pyvenv_version(cfg_path: &Path) -> String {
    let Ok(cfg) = std::fs::read_to_string(cfg_path) else {
        return String::new();
    };
    for line in cfg.lines() {
        if let Some((key, val)) = line.split_once('=')
            && key.trim() == "version"
        {
            let v = val.trim().to_string();
            if !v.is_empty() {
                return v;
            }
        }
    }
    String::new()
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
