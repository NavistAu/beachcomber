//! Resolution: `bc_resolve`, `bc_eval`. Task 3.5 of
//! docs/superpowers/plans/2026-08-15-client-abi-and-sdk-refactor.md.
//!
//! Mirrors all four of Task 1.6's properties
//! (libbeachcomber/tests/resolution.rs) through the ABI: cache-ref virtual
//! field, env-or-cache cascade including miss -> "", a path expression over
//! a supplied cwd, and the truncate/basename filters. Runs against a real
//! in-process daemon so cache-ref virtual fields have something to fetch.

mod common;
use common::daemon::DaemonGuard;

use std::ffi::{CStr, CString, c_char};
use std::path::Path;
use std::ptr;

use beachcomber::envelope::bc_string_free;
use beachcomber::{BcClient, bc_client_free, bc_client_new, bc_eval, bc_put, bc_resolve};

fn client_for(sock: &Path) -> *mut BcClient {
    let json = format!(
        r#"{{"socket_path":"{}","autostart":false,"timeout_ms":2000}}"#,
        sock.display()
    );
    let cs = CString::new(json).unwrap();
    unsafe { bc_client_new(cs.as_ptr()) }
}

fn cs(s: &str) -> CString {
    CString::new(s).unwrap()
}

fn read(ptr: *mut c_char) -> serde_json::Value {
    assert!(!ptr.is_null(), "call_ffi must never return NULL");
    let body = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_string();
    unsafe { bc_string_free(ptr) };
    serde_json::from_str(&body).expect("envelope must be valid JSON")
}

/// Case 1: a virtual field whose expression references a `cache.*` value
/// resolves to that value.
#[test]
fn resolve_cache_ref_virtual_field() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);

    let provider_key = cs("otherprov");
    let data = cs(r#"{"otherfield":"hello-value"}"#);
    let put_v = read(unsafe {
        bc_put(
            client,
            provider_key.as_ptr(),
            data.as_ptr(),
            ptr::null(),
            ptr::null(),
        )
    });
    assert_eq!(put_v["ok"], serde_json::json!(true));

    let key = cs("myprov.myfield");
    let cwd = cs("/tmp");
    let overrides = cs(r#"{"myprov.myfield":"cache.otherprov.otherfield"}"#);
    let v = read(unsafe {
        bc_resolve(
            client,
            key.as_ptr(),
            cwd.as_ptr(),
            ptr::null(),
            overrides.as_ptr(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!("hello-value"));

    unsafe { bc_client_free(client) };
}

/// Case 2: a cascade `env.X or cache.p.f` takes the env term when present
/// and falls through when absent; a total miss yields `""`, not an error.
#[test]
fn resolve_env_or_cache_cascade_falls_through_to_empty_string_on_miss() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let key = cs("terraform.workspace"); // built-in: "env.TF_WORKSPACE or cache.terraform.workspace"
    let cwd = cs("/tmp");

    // (a) env present -> env term wins, no daemon fetch needed.
    let env_present = cs(r#"{"TF_WORKSPACE":"prod-env"}"#);
    let v = read(unsafe {
        bc_resolve(
            client,
            key.as_ptr(),
            cwd.as_ptr(),
            env_present.as_ptr(),
            ptr::null(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!("prod-env"));

    // (b) both absent -> a miss yields "" rather than an error.
    let v =
        read(unsafe { bc_resolve(client, key.as_ptr(), cwd.as_ptr(), ptr::null(), ptr::null()) });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!(""));

    unsafe { bc_client_free(client) };
}

/// Case 3: a path expression evaluated over a supplied `cwd` selects the
/// expected cache coordinate.
#[test]
fn resolve_path_expression_over_supplied_cwd() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let key = cs("myproject");
    let overrides =
        cs(r#"{"myproject":"'workspace-a' if cwd == '/Users/x/repo-a' else 'workspace-b'"}"#);

    let cwd_a = cs("/Users/x/repo-a");
    let v = read(unsafe {
        bc_resolve(
            client,
            key.as_ptr(),
            cwd_a.as_ptr(),
            ptr::null(),
            overrides.as_ptr(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!("workspace-a"));

    let cwd_b = cs("/Users/x/repo-b");
    let v = read(unsafe {
        bc_resolve(
            client,
            key.as_ptr(),
            cwd_b.as_ptr(),
            ptr::null(),
            overrides.as_ptr(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!("workspace-b"));

    unsafe { bc_client_free(client) };
}

/// Case 4: an expression using the `truncate` and `basename` filters
/// resolves through `bc_eval` — proving the filters survived the move into
/// the client crate, not just the symbols.
#[test]
fn eval_truncate_and_basename_filters() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let cwd = cs("/tmp");
    let env = cs(r#"{"LONGVAR":"abcdefghij","PYVAR":"/foo/bar/baz"}"#);

    let truncate_expr = cs("env.LONGVAR | truncate(5)");
    let v = read(unsafe {
        bc_eval(
            client,
            truncate_expr.as_ptr(),
            cwd.as_ptr(),
            env.as_ptr(),
            ptr::null(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!("abcde..."));

    let basename_expr = cs("env.PYVAR | basename");
    let v = read(unsafe {
        bc_eval(
            client,
            basename_expr.as_ptr(),
            cwd.as_ptr(),
            env.as_ptr(),
            ptr::null(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!("baz"));

    unsafe { bc_client_free(client) };
}

/// `cwd` is required: NULL must return an error envelope, never fall back
/// to the process's own working directory.
#[test]
fn resolve_null_cwd_is_an_error_envelope() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let key = cs("terraform.workspace");

    let v =
        read(unsafe { bc_resolve(client, key.as_ptr(), ptr::null(), ptr::null(), ptr::null()) });
    assert_eq!(v["ok"], serde_json::json!(false));

    unsafe { bc_client_free(client) };
}

#[test]
fn eval_null_cwd_is_an_error_envelope() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let expr = cs("env.LONGVAR | truncate(5)");

    let v = read(unsafe { bc_eval(client, expr.as_ptr(), ptr::null(), ptr::null(), ptr::null()) });
    assert_eq!(v["ok"], serde_json::json!(false));

    unsafe { bc_client_free(client) };
}
