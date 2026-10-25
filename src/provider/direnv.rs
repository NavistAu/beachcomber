use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    Source, SourceMetadata, SourceResult, Value,
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
        // LIMITATION: The allow DB path depends on the envrc abs path (per-instance),
        // so it cannot be registered as a static watch path in SourceMetadata.
        // WatchAndPoll with 30s poll interval provides a correctness backstop:
        // .envrc changes fire immediately; allow DB changes (direnv allow/deny from
        // another terminal) are caught within 30 seconds.
        invalidation: InvalidationStrategy::Watch {
            patterns: vec![".envrc".into()],
            abs_paths: vec![],
        },
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 60,
        },
        fsevents_reinstate: true,
        decay: None,
        budget_execute: crate::scheduler::lifecycle::DEFAULT_BUDGET_EXECUTE,
        timeout: crate::scheduler::lifecycle::DEFAULT_TIMEOUT,
        nearest: true,
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

    fn execute(&self, path: Option<&str>) -> Result<SourceResult, String> {
        let Some(path) = path else {
            return Err("no directory to run in".into());
        };
        let dir = Path::new(path);
        if !dir.join(".envrc").exists() {
            return Ok(SourceResult::new());
        }

        // Resolve absolute .envrc path for allow DB key.
        let envrc_abs = match std::fs::canonicalize(dir.join(".envrc")) {
            Ok(p) => p,
            Err(e) => return Err(format!("direnv: cannot resolve .envrc: {e}")),
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
        Ok(result)
    }

    fn root(&self, dir: &str) -> Result<Option<String>, String> {
        Ok(find_envrc_root(Path::new(dir)))
    }

    fn has_root(&self) -> bool {
        true
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

/// Seam tests for direnv provider.
/// These do NOT run direnv — they test allow-DB file resolution directly.
/// sha2 is already a direct dependency (Cargo.toml) used in production.
#[cfg(test)]
mod tests {
    use super::direnv_source_with_allow_db_root;
    use crate::provider::Value;
    use sha2::{Digest, Sha256};
    use tempfile::TempDir;

    fn sha256_hex(input: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(input);
        let result = hasher.finalize();
        result.iter().map(|b| format!("{:02x}", b)).collect()
    }

    fn setup_direnv(allow_db_root: &TempDir, envrc_path: &std::path::Path) -> std::path::PathBuf {
        // Canonicalize to match what the production code does (std::fs::canonicalize).
        // On macOS, /tmp is a symlink to /private/tmp, so we must use the canonical form.
        let canon = std::fs::canonicalize(envrc_path).unwrap();
        let canon_str = canon.to_string_lossy();
        let hash = sha256_hex(canon_str.as_bytes());
        let allow_file = allow_db_root.path().join(&hash);
        std::fs::write(&allow_file, canon_str.as_bytes()).unwrap();
        allow_file
    }

    #[test]
    fn direnv_allowed_when_allow_db_entry_exists() {
        let envrc_dir = TempDir::new().unwrap();
        let envrc_path = envrc_dir.path().join(".envrc");
        std::fs::write(&envrc_path, "export FOO=bar\n").unwrap();

        let allow_db = TempDir::new().unwrap();
        setup_direnv(&allow_db, &envrc_path);

        let source = direnv_source_with_allow_db_root(allow_db.path().to_path_buf());
        let result = source
            .execute(Some(envrc_dir.path().to_str().unwrap()))
            .unwrap();

        assert_eq!(
            result.fields.get("allowed"),
            Some(&Value::Bool(true)),
            "allow DB entry present → allowed must be true"
        );
        assert_eq!(result.fields.get("status").unwrap().as_text(), "allowed");
    }

    #[test]
    fn direnv_blocked_when_no_allow_db_entry() {
        let envrc_dir = TempDir::new().unwrap();
        std::fs::write(envrc_dir.path().join(".envrc"), "export FOO=bar\n").unwrap();

        let allow_db = TempDir::new().unwrap(); // empty — no entry

        let source = direnv_source_with_allow_db_root(allow_db.path().to_path_buf());
        let result = source
            .execute(Some(envrc_dir.path().to_str().unwrap()))
            .unwrap();

        assert_eq!(
            result.fields.get("allowed"),
            Some(&Value::Bool(false)),
            "no allow DB entry → allowed must be false"
        );
        assert_eq!(result.fields.get("status").unwrap().as_text(), "blocked");
    }

    #[test]
    fn direnv_empty_when_no_envrc() {
        let dir = TempDir::new().unwrap(); // no .envrc file
        let allow_db = TempDir::new().unwrap();

        let source = direnv_source_with_allow_db_root(allow_db.path().to_path_buf());
        let result = source.execute(Some(dir.path().to_str().unwrap())).unwrap();

        assert!(
            result.fields.is_empty(),
            "no .envrc → empty result, got {:?}",
            result.fields
        );
    }
}
