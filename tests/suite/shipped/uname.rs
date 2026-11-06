//! `defaults/uname.toml`: the kernel, from `sys`.

use super::scenario;
use serde_json::json;

#[test]
fn uname_provider_executes() {
    let s = scenario();
    let system = s.get("uname", &s.project).expect("uname answers");
    let want = if cfg!(target_os = "macos") {
        "Darwin"
    } else {
        "Linux"
    };
    assert_eq!(system["sysname"], json!(want), "{system}");
    for field in ["release", "version", "machine"] {
        assert!(
            system[field].as_str().is_some_and(|v| !v.is_empty()),
            "{field}: {system}"
        );
    }
}

/// The deliberate difference: polled once an hour while read, where the Rust
/// Source ran once.
#[test]
fn uname_is_polled_hourly() {
    let s = scenario();
    // A cache leaf describes a record: the read makes it.
    assert!(s.get("uname.sysname", &s.project).is_some());
    let leaf = s
        .get("uname:cache.sysname", &s.project)
        .expect("a cache leaf");
    assert_eq!(
        leaf["poll"],
        json!({"interval": "3600s", "count": 1}),
        "{leaf}"
    );
}
