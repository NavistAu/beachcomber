use beachcomber::provider::Provider;
use beachcomber::provider::SourceScope;
use beachcomber::provider::Value;
use beachcomber::provider::sudo::{SudoProvider, sudo_active_with_ts_path};

#[test]
fn sudo_provider_metadata() {
    let p = SudoProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "sudo");
    assert_eq!(meta.sources.len(), 1);
    let src = &meta.sources[0];
    assert_eq!(src.name, "state");
    assert_eq!(src.scope, SourceScope::Global);
    assert_eq!(src.fields.len(), 1);
    assert_eq!(src.fields[0].name, "active");
}

#[test]
fn sudo_provider_executes() {
    let p = SudoProvider;
    let sources = p.sources();
    let result = sources[0].execute(None);
    // sudo state returns active field when timestamp is readable, or empty when root-only.
    // Both are valid outcomes — we just check it doesn't panic.
    let _ = result; // no-panic check
}

#[test]
fn sudo_unreadable_path_produces_no_active_field() {
    // A path that exists but is unreadable (or simply doesn't exist like a root-only file).
    // We simulate a non-existent path — same error class as permission-denied from the
    // daemon's perspective (read_dir fails → unknown state).
    let result = sudo_active_with_ts_path(std::path::Path::new("/nonexistent/sudo/ts/path"));
    assert!(
        !result.fields.contains_key("active"),
        "unreadable sudo timestamp path must omit 'active' field, got: {:?}",
        result.fields
    );
}

#[test]
fn sudo_readable_expired_timestamp_is_false() {
    let dir = tempfile::tempdir().unwrap();
    let ts_file = dir.path().join("jhogendorn");
    std::fs::write(&ts_file, b"").unwrap();
    // Set mtime to 10 minutes ago using touch(1).
    // macOS uses `date -v-10M`, Linux uses `date -d '-10 minutes'` — || fallback handles both.
    let status = std::process::Command::new("sh")
        .args([
            "-c",
            &format!(
                "touch -t $(date -v-10M +%Y%m%d%H%M.%S 2>/dev/null || date -d '-10 minutes' +%Y%m%d%H%M.%S) '{}'",
                ts_file.to_str().unwrap()
            ),
        ])
        .status()
        .expect("touch");
    assert!(status.success(), "touch failed to set old mtime");

    let result = sudo_active_with_ts_path(dir.path());
    assert_eq!(
        result.fields.get("active"),
        Some(&Value::Bool(false)),
        "expired timestamp → active = false"
    );
}

#[test]
fn sudo_readable_fresh_timestamp_is_true() {
    let dir = tempfile::tempdir().unwrap();
    let ts_file = dir.path().join("jhogendorn");
    std::fs::write(&ts_file, b"").unwrap();
    // mtime defaults to now → within 5-min window.

    let result = sudo_active_with_ts_path(dir.path());
    assert_eq!(
        result.fields.get("active"),
        Some(&Value::Bool(true)),
        "fresh timestamp → active = true"
    );
}
