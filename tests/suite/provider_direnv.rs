/// Seam tests for direnv provider.
/// These do NOT run direnv — they test allow-DB file resolution directly.
/// sha2 is already a direct dependency (Cargo.toml) used in production.
use beachcomber::provider::Value;
use beachcomber::provider::direnv::direnv_source_with_allow_db_root;
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
    let result = source.execute(Some(envrc_dir.path().to_str().unwrap()));

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
    let result = source.execute(Some(envrc_dir.path().to_str().unwrap()));

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
    let result = source.execute(Some(dir.path().to_str().unwrap()));

    assert!(
        result.fields.is_empty(),
        "no .envrc → empty result, got {:?}",
        result.fields
    );
}
