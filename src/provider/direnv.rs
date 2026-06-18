use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub struct DirenvProvider;

impl Provider for DirenvProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "direnv".into(),
            sources: vec![state_source_metadata()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(DirenvState {
            allow_db_root: None,
        })]
    }
}

fn state_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "state".into(),
        fields: vec![
            FieldSchema {
                name: "status".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "allowed".into(),
                field_type: FieldType::Bool,
            },
        ],
        scope: SourceScope::PathScoped,
        // LIMITATION: The allow DB path depends on the envrc abs path (per-instance),
        // so it cannot be registered as a static watch path in SourceMetadata.
        // WatchAndPoll with 30s poll interval provides a correctness backstop:
        // .envrc changes fire immediately; allow DB changes (direnv allow/deny from
        // another terminal) are caught within 30 seconds.
        invalidation: InvalidationStrategy::Watch {
            patterns: vec![".envrc".into()],
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

struct DirenvState {
    /// Override for the allow DB root directory. When `None`, uses the XDG default.
    /// Intended for test injection.
    allow_db_root: Option<PathBuf>,
}

impl Source for DirenvState {
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
        if !dir.join(".envrc").exists() {
            return SourceResult::new();
        }

        // Resolve absolute .envrc path for allow DB key.
        let envrc_abs = match std::fs::canonicalize(dir.join(".envrc")) {
            Ok(p) => p,
            Err(_) => return SourceResult::new(),
        };
        let envrc_abs_str = envrc_abs.to_string_lossy();

        // Determine allow DB root.
        let allow_db_root = self
            .allow_db_root
            .clone()
            .unwrap_or_else(default_allow_db_root);

        // Hash the abs path (direnv uses sha256 of the path string).
        let hash = sha256_hex(envrc_abs_str.as_bytes());
        let allowed = allow_db_root.join(&hash).exists();

        let status = if allowed { "allowed" } else { "blocked" };

        let mut result = SourceResult::new();
        result.insert("status", Value::String(status.to_string()));
        result.insert("allowed", Value::Bool(allowed));
        result
    }

    fn canonical_path(&self, path: Option<&str>) -> Option<String> {
        let p = path?;
        find_envrc_root(Path::new(p))
    }
}

fn sha256_hex(data: &[u8]) -> String {
    // sha2 is a direct dep (Cargo.toml). No external vendoring needed.
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect()
}

fn default_allow_db_root() -> PathBuf {
    // direnv allow DB: ${XDG_DATA_HOME:-$HOME/.local/share}/direnv/allow
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        return PathBuf::from(xdg).join("direnv").join("allow");
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("direnv")
        .join("allow")
}

fn find_envrc_root(start: &Path) -> Option<String> {
    let mut cur: Option<&Path> = Some(start);
    while let Some(dir) = cur {
        if dir.join(".envrc").exists() {
            return Some(dir.to_string_lossy().to_string());
        }
        cur = dir.parent();
    }
    None
}

/// Construct the direnv source reading from an explicit allow DB root.
/// Intended for seam tests — bypasses XDG_DATA_HOME and ~/.local/share.
#[cfg(any(test, feature = "test-helpers"))]
pub fn direnv_source_with_allow_db_root(root: PathBuf) -> Box<dyn Source> {
    Box::new(DirenvState {
        allow_db_root: Some(root),
    })
}
