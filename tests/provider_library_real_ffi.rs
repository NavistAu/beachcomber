/// Real FFI seam tests for `src/boundaries/library.rs`.
///
/// Loads the `test_provider_lib` cdylib fixture through the real
/// `LibloadingLoader` implementation and exercises every method on
/// `LoadedLibrary`.  Coverage target: push `boundaries/library.rs` from 0%
/// to 75%+.
///
/// Test inventory:
/// 1. `source_count_returns_two`          — `bc_source_count` present, returns 2
/// 2. `call_source_metadata_source0`      — `bc_source_metadata(0)` returns valid JSON
/// 3. `call_source_metadata_source1`      — `bc_source_metadata(1)` returns valid JSON
/// 4. `call_source_metadata_out_of_range` — `bc_source_metadata(99)` returns None (null ptr)
/// 5. `call_source_execute_source0`       — `bc_source_execute(0, None)` returns JSON
/// 6. `call_source_execute_source1_path`  — `bc_source_execute(1, Some(path))` echoes path
/// 7. `call_source_execute_out_of_range`  — `bc_source_execute(99, None)` returns None
/// 8. `call_metadata_legacy_symbol`       — `beachcomber_provider_metadata` via `call_metadata`
/// 9. `call_execute_legacy_symbol`        — `beachcomber_provider_execute` via `call_execute`
/// 10. `call_metadata_missing_symbol`     — absent symbol returns None
/// 11. `call_execute_missing_symbol`      — absent symbol returns None
/// 12. `call_metadata_null_return`        — symbol present but returns null → None
/// 13. `call_execute_null_return`         — symbol present but returns null → None
/// 14. `call_metadata_malformed_json`     — returns invalid JSON string (not None)
/// 15. `call_execute_malformed_json`      — returns invalid JSON string (not None)
/// 16. `load_nonexistent_path`            — LibloadingLoader::load fails gracefully
use beachcomber::boundaries::library::{LibloadingLoader, LibraryLoader};

// ── dylib path helpers ────────────────────────────────────────────────────────

const LIB_NAME: &str = if cfg!(target_os = "macos") {
    "libtest_provider_lib.dylib"
} else if cfg!(target_os = "linux") {
    "libtest_provider_lib.so"
} else {
    "libtest_provider_lib.so" // best-effort fallback
};

fn fixture_dylib_path() -> std::path::PathBuf {
    let target_dir = std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".into());
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    std::path::PathBuf::from(target_dir)
        .join(profile)
        .join(LIB_NAME)
}

fn ensure_fixture_built() -> std::path::PathBuf {
    let p = fixture_dylib_path();
    if !p.exists() {
        let out = std::process::Command::new("cargo")
            .args(["build", "-p", "test_provider_lib"])
            .output()
            .expect("cargo build invocation failed");
        assert!(
            out.status.success(),
            "fixture build failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert!(p.exists(), "fixture artifact missing at {p:?}");
    p
}

/// Load the fixture once and return the loaded library.
fn load_fixture() -> Box<dyn beachcomber::boundaries::library::LoadedLibrary> {
    let path = ensure_fixture_built();
    LibloadingLoader
        .load(path.to_string_lossy().into_owned())
        .expect("LibloadingLoader::load should succeed for valid fixture")
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[test]
fn source_count_returns_two() {
    let lib = load_fixture();
    assert_eq!(lib.source_count(), 2, "bc_source_count should return 2");
}

#[test]
fn call_source_metadata_source0() {
    let lib = load_fixture();
    let raw = lib.call_source_metadata(0);
    assert!(raw.is_some(), "bc_source_metadata(0) should return Some");
    let json = raw.unwrap();
    // The fixture returns the alpha source with "name":"alpha".
    assert!(
        json.contains("\"alpha\""),
        "metadata JSON should contain source name 'alpha', got: {json}"
    );
    // Verify it is valid JSON.
    let parsed: serde_json::Value =
        serde_json::from_str(&json).expect("bc_source_metadata(0) should return valid JSON");
    assert_eq!(parsed["name"], "alpha");
    assert_eq!(parsed["global"], true);
}

#[test]
fn call_source_metadata_source1() {
    let lib = load_fixture();
    let raw = lib.call_source_metadata(1);
    assert!(raw.is_some(), "bc_source_metadata(1) should return Some");
    let json = raw.unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&json).expect("bc_source_metadata(1) should return valid JSON");
    assert_eq!(parsed["name"], "beta");
    assert_eq!(parsed["global"], false);
}

#[test]
fn call_source_metadata_out_of_range() {
    let lib = load_fixture();
    let result = lib.call_source_metadata(99);
    assert!(
        result.is_none(),
        "bc_source_metadata(99) should return None (null ptr from fixture)"
    );
}

#[test]
fn call_source_execute_source0() {
    let lib = load_fixture();
    let result = lib.call_source_execute(0, None);
    assert!(
        result.is_some(),
        "bc_source_execute(0, None) should return Some"
    );
    let json = result.unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&json).expect("bc_source_execute(0) should return valid JSON");
    assert_eq!(parsed["value"], "hello");
    assert_eq!(parsed["count"], 42);
}

#[test]
fn call_source_execute_source1_path() {
    let lib = load_fixture();
    let result = lib.call_source_execute(1, Some("/home/user".to_string()));
    assert!(
        result.is_some(),
        "bc_source_execute(1, path) should return Some"
    );
    let json = result.unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&json).expect("bc_source_execute(1) should return valid JSON");
    assert_eq!(parsed["flag"], true);
    // The fixture echoes the path argument back.
    assert_eq!(parsed["path"], "/home/user");
}

#[test]
fn call_source_execute_out_of_range() {
    let lib = load_fixture();
    let result = lib.call_source_execute(99, None);
    assert!(
        result.is_none(),
        "bc_source_execute(99, None) should return None (null ptr from fixture)"
    );
}

#[test]
fn call_metadata_legacy_symbol() {
    let lib = load_fixture();
    let result = lib.call_metadata("beachcomber_provider_metadata".to_string());
    assert!(
        result.is_some(),
        "call_metadata for legacy symbol should return Some"
    );
    let json = result.unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&json).expect("legacy metadata should be valid JSON");
    assert_eq!(parsed["name"], "legacy");
}

#[test]
fn call_execute_legacy_symbol() {
    let lib = load_fixture();
    let result = lib.call_execute("beachcomber_provider_execute".to_string(), None);
    assert!(
        result.is_some(),
        "call_execute for legacy symbol should return Some"
    );
    let json = result.unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&json).expect("legacy execute should return valid JSON");
    assert_eq!(parsed["info"], "legacy_result");
}

#[test]
fn call_metadata_missing_symbol() {
    let lib = load_fixture();
    // "bc_no_such_symbol" is not exported by the fixture.
    let result = lib.call_metadata("bc_no_such_symbol".to_string());
    assert!(
        result.is_none(),
        "call_metadata for absent symbol should return None"
    );
}

#[test]
fn call_execute_missing_symbol() {
    let lib = load_fixture();
    let result = lib.call_execute("bc_no_such_execute".to_string(), None);
    assert!(
        result.is_none(),
        "call_execute for absent symbol should return None"
    );
}

#[test]
fn call_metadata_null_return() {
    let lib = load_fixture();
    // bc_metadata_returns_null is exported but always returns a null pointer.
    let result = lib.call_metadata("bc_metadata_returns_null".to_string());
    assert!(
        result.is_none(),
        "call_metadata for null-returning symbol should return None"
    );
}

#[test]
fn call_execute_null_return() {
    let lib = load_fixture();
    let result = lib.call_execute("bc_execute_returns_null".to_string(), None);
    assert!(
        result.is_none(),
        "call_execute for null-returning symbol should return None"
    );
}

#[test]
fn call_metadata_malformed_json() {
    let lib = load_fixture();
    // bc_metadata_malformed returns "{bad json}" — a non-null, non-empty C string
    // that is not valid JSON.  The loader should still return Some (it just
    // reads the string), leaving JSON parsing to the caller.
    let result = lib.call_metadata("bc_metadata_malformed".to_string());
    assert!(
        result.is_some(),
        "call_metadata for malformed-JSON symbol should return Some (string reading succeeds)"
    );
    let s = result.unwrap();
    assert!(
        serde_json::from_str::<serde_json::Value>(&s).is_err(),
        "the returned string should not be valid JSON: {s}"
    );
}

#[test]
fn call_execute_malformed_json() {
    let lib = load_fixture();
    let result = lib.call_execute("bc_execute_malformed".to_string(), None);
    assert!(
        result.is_some(),
        "call_execute for malformed-JSON symbol should return Some"
    );
    let s = result.unwrap();
    assert!(
        serde_json::from_str::<serde_json::Value>(&s).is_err(),
        "the returned string should not be valid JSON: {s}"
    );
}

#[test]
fn load_nonexistent_path() {
    let result = LibloadingLoader.load("/nonexistent/path/libfoo.dylib".to_string());
    assert!(
        result.is_err(),
        "LibloadingLoader::load should fail for a nonexistent path"
    );
    // Extract the error string without requiring Debug on Box<dyn LoadedLibrary>.
    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("expected Err but got Ok"),
    };
    assert!(
        err.contains("failed to load library"),
        "error message should mention 'failed to load library', got: {err}"
    );
}
