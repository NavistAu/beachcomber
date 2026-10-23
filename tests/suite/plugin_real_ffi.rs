//! `plugin()` through a real `dlopen` of the `test_provider_lib` cdylib
//! fixture, by way of the real `LibloadingLoader`.
//!
//! - `plugin_calls_a_real_cdylib` — `bc_sum` and `bc_echo_args`
//! - `plugin_failures_are_distinct_through_a_real_cdylib` — a missing symbol,
//!   a NULL return, invalid JSON and output that is not UTF-8 each have their
//!   own message
//! - `one_file_is_one_image_whatever_the_name` — symlinks and hard links to one
//!   file report one loaded image
//! - `load_nonexistent_path` — loading a path with no library fails
//!   gracefully

use crate::common::fixture::ensure_fixture_built;
use beachcomber::boundaries::library::{LibloadingLoader, LibraryLoader};

#[test]
fn load_nonexistent_path() {
    let result = LibloadingLoader.load("/nonexistent/path/libfoo.dylib".to_string());
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

#[test]
fn plugin_calls_a_real_cdylib() {
    use beachcomber::engine::{PluginSet, daemon_engine_env};
    use libbeachcomber::engine::EngineCtx;
    use std::sync::Arc;

    // A `/` in the name is the library path, which must be absolute.
    let path = ensure_fixture_built();
    let plugins = Arc::new(PluginSet::new(vec![], Arc::new(LibloadingLoader)));
    let env = daemon_engine_env(Arc::new(EngineCtx::default()), plugins);
    let p = path.to_string_lossy();
    let sum = env
        .render_str(&format!("{{{{ plugin('{p}', 'bc_sum', 1, 2, 3.5) }}}}"), ())
        .expect("plugin bc_sum");
    assert_eq!(sum, "6.5");
    let echoed = env
        .render_str(
            &format!(
                "{{% set r = plugin('{p}', 'bc_echo_args', 1, 'a') %}}{{{{ r | length }}}}:{{{{ r[0] }}}}:{{{{ r[1] }}}}"
            ),
            (),
        )
        .expect("plugin bc_echo_args");
    assert_eq!(echoed, "2:1:a");
}

#[test]
fn plugin_failures_are_distinct_through_a_real_cdylib() {
    use beachcomber::engine::{PluginSet, daemon_engine_env};
    use libbeachcomber::engine::EngineCtx;
    use std::sync::Arc;

    let path = ensure_fixture_built();
    let plugins = Arc::new(PluginSet::new(vec![], Arc::new(LibloadingLoader)));
    let env = daemon_engine_env(Arc::new(EngineCtx::default()), plugins);
    let p = path.to_string_lossy();
    let fails = |symbol: &str| {
        env.render_str(&format!("{{{{ plugin('{p}', '{symbol}') }}}}"), ())
            .unwrap_err()
            .to_string()
    };
    let missing = fails("bc_no_such_symbol");
    assert!(missing.contains("symbol not found"), "{missing}");
    let null = fails("bc_plugin_returns_null");
    assert!(null.contains("returned NULL"), "{null}");
    let bad = fails("bc_plugin_returns_malformed");
    assert!(bad.contains("returned invalid JSON"), "{bad}");
    // The strict-UTF-8 boundary: invalid bytes are an error, not U+FFFD.
    let utf8 = fails("bc_plugin_returns_invalid_utf8");
    assert!(utf8.contains("not UTF-8"), "{utf8}");
}

#[test]
fn one_file_is_one_image_whatever_the_name() {
    let fixture = ensure_fixture_built();
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.lib");
    let b = dir.path().join("b.lib");
    std::os::unix::fs::symlink(&fixture, &a).unwrap();
    std::fs::hard_link(&fixture, &b).unwrap_or_else(|_| {
        // A different filesystem: a second symlink is the same file too.
        std::os::unix::fs::symlink(&fixture, &b).unwrap();
    });
    // Every handle is held until the comparisons are done: a library that was
    // unloaded could hand its address to the next one and fake a match.
    let load = |p: &std::path::Path| {
        LibloadingLoader
            .load(p.to_string_lossy().into_owned())
            .map_err(|e| e.to_string())
            .unwrap()
    };
    let copy = dir.path().join("copy.lib");
    std::fs::copy(&fixture, &copy).unwrap();
    let (la, lb, lf, lc) = (load(&a), load(&b), load(&fixture), load(&copy));
    assert_eq!(la.identity(), lb.identity());
    assert_eq!(la.identity(), lf.identity());
    // A copy is another file, so another image.
    assert_ne!(lc.identity(), lf.identity());
}
