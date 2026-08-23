//! Operations: `bc_get`, `bc_put`, `bc_put_null`, `bc_refresh`, `bc_status`,
//! `bc_introspect`, `bc_hello`. Task 3.4 of
//! docs/superpowers/plans/2026-08-15-client-abi-and-sdk-refactor.md.
//!
//! Runs against a real in-process daemon spawned by `tests/common/daemon.rs`.

mod common;
use common::daemon::DaemonGuard;

use std::ffi::{CStr, CString, c_char};
use std::path::Path;
use std::ptr;
use std::time::Duration;

use beachcomber::envelope::bc_string_free;
use beachcomber::{
    BC_GET_FORCE, BC_GET_WAIT, BcClient, bc_client_free, bc_client_new, bc_get, bc_hello,
    bc_introspect, bc_put, bc_put_null, bc_refresh, bc_status,
};

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

#[test]
fn get_returns_ok_envelope_for_a_real_key() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let key = cs("hostname.short");

    let v = read(unsafe { bc_get(client, key.as_ptr(), ptr::null(), 0) });
    assert_eq!(v["ok"], serde_json::json!(true));

    unsafe { bc_client_free(client) };
}

#[test]
fn get_reserved_flag_bit_is_rejected_not_ignored() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let key = cs("hostname.short");

    let v = read(unsafe { bc_get(client, key.as_ptr(), ptr::null(), 1 << 5) });
    assert_eq!(v["ok"], serde_json::json!(false));
    assert_eq!(v["error"]["kind"], serde_json::json!("bad_flags"));

    unsafe { bc_client_free(client) };
}

#[test]
fn get_accepts_the_documented_flag_combination() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let key = cs("hostname.short");

    let v = read(unsafe {
        bc_get(
            client,
            key.as_ptr(),
            ptr::null(),
            BC_GET_FORCE | BC_GET_WAIT,
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true));

    unsafe { bc_client_free(client) };
}

#[test]
fn put_get_put_null_round_trip() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    // `put`'s key names the virtual provider; `data`'s top-level keys become
    // its fields (see tests/conformance/put/nested_object_data.json).
    let provider_key = cs("myvirtual");
    let field_key = cs("myvirtual.field");
    let data = cs(r#"{"field":"hello"}"#);

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

    let get_v = read(unsafe { bc_get(client, field_key.as_ptr(), ptr::null(), 0) });
    assert_eq!(get_v["ok"], serde_json::json!(true));
    assert_eq!(get_v["data"]["data"], serde_json::json!("hello"));

    let put_null_v = read(unsafe { bc_put_null(client, provider_key.as_ptr(), ptr::null()) });
    assert_eq!(put_null_v["ok"], serde_json::json!(true));

    unsafe { bc_client_free(client) };
}

#[test]
fn refresh_returns_ok_envelope() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let key = cs("hostname.short");

    let v = read(unsafe { bc_refresh(client, key.as_ptr(), ptr::null()) });
    assert_eq!(v["ok"], serde_json::json!(true));

    unsafe { bc_client_free(client) };
}

#[test]
fn status_returns_an_array_envelope() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);

    let v = read(unsafe { bc_status(client) });
    assert_eq!(v["ok"], serde_json::json!(true));
    assert!(v["data"].is_array());

    unsafe { bc_client_free(client) };
}

#[test]
fn introspect_daemon_subject() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let subject = cs("daemon");

    let v = read(unsafe { bc_introspect(client, subject.as_ptr(), ptr::null()) });
    assert_eq!(v["ok"], serde_json::json!(true));
    assert!(v["data"]["pid"].is_number());
    assert!(v["data"]["version"].is_string());

    unsafe { bc_client_free(client) };
}

#[test]
fn introspect_procs_subject_honours_duration_secs_option() {
    // The `procs` subject samples exec events for `duration_secs` before
    // reporting, succeeding or not depending on OS tracing privileges
    // (`eslogger` on macOS, akin to `uptime_provider_executes` needing an
    // unsandboxed environment). What this test can assert regardless of
    // that: the option actually reached the daemon rather than being
    // dropped — proven by the call taking ~1s (the requested duration), not
    // the 2s default `handle_introspect_procs` falls back to when
    // `duration_secs` is absent.
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let subject = cs("procs");
    let options = cs(r#"{"duration_secs":1}"#);

    let start = std::time::Instant::now();
    let v = read(unsafe { bc_introspect(client, subject.as_ptr(), options.as_ptr()) });
    let elapsed = start.elapsed();

    assert!(v["ok"].is_boolean(), "envelope must have an ok field: {v}");
    assert!(
        elapsed < Duration::from_millis(1800),
        "duration_secs=1 must not fall back to the 2s default; took {elapsed:?}"
    );

    unsafe { bc_client_free(client) };
}

#[test]
fn hello_returns_both_versions() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);

    let v = read(unsafe { bc_hello(client) });
    assert_eq!(v["ok"], serde_json::json!(true));
    assert!(v["data"]["protocol_version"].is_string());
    assert!(v["data"]["daemon_version"].is_string());

    unsafe { bc_client_free(client) };
}
