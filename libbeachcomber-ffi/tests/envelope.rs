//! Envelope shape, error kind slugs, the panic boundary, and pointer
//! lifecycle. Task 3.2 of
//! docs/superpowers/plans/2026-08-15-client-abi-and-sdk-refactor.md.

use std::ffi::CStr;
use std::os::raw::c_char;
use std::panic::AssertUnwindSafe;

use beachcomber::envelope::{ErrorKind, FfiError, bc_string_free, call_ffi};
use serde_json::json;

/// Reads a `char *` envelope into an owned `String` without freeing it.
fn read(ptr: *mut c_char) -> String {
    assert!(!ptr.is_null(), "call_ffi must never return NULL");
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

#[test]
fn success_round_trips() {
    let ptr = call_ffi(|| Ok(json!({"key": "value"})));
    let body = read(ptr);
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["ok"], json!(true));
    assert_eq!(parsed["data"], json!({"key": "value"}));
    unsafe { bc_string_free(ptr) };
}

#[test]
fn every_error_kind_serialises_its_slug() {
    let cases = [
        (ErrorKind::BadFlags, "bad_flags"),
        (ErrorKind::Busy, "busy"),
        (ErrorKind::Panic, "panic"),
        (ErrorKind::VersionSkew, "version_skew"),
        (ErrorKind::DaemonNotRunning, "daemon_not_running"),
        (ErrorKind::ConnectionFailed, "connection_failed"),
        (ErrorKind::IoError, "io_error"),
        (ErrorKind::ParseError, "parse_error"),
        (ErrorKind::ServerError, "server_error"),
        (ErrorKind::Timeout, "timeout"),
    ];
    for (kind, slug) in cases {
        let ptr = call_ffi(move || Err(FfiError::new(kind, "boom")));
        let body = read(ptr);
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["ok"], json!(false), "kind {slug}");
        assert_eq!(parsed["error"]["kind"], json!(slug), "kind {slug}");
        assert_eq!(parsed["error"]["message"], json!("boom"), "kind {slug}");
        unsafe { bc_string_free(ptr) };
    }
}

#[test]
fn panic_is_caught_and_reported_as_panic_kind() {
    let ptr = call_ffi(AssertUnwindSafe(
        || -> Result<serde_json::Value, FfiError> {
            panic!("kaboom");
        },
    ));
    let body = read(ptr);
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["ok"], json!(false));
    assert_eq!(parsed["error"]["kind"], json!("panic"));
    assert_eq!(parsed["error"]["message"], json!("kaboom"));
    unsafe { bc_string_free(ptr) };
}

#[test]
fn returned_pointer_is_freeable() {
    let ptr = call_ffi(|| Ok(json!(42)));
    unsafe { bc_string_free(ptr) };
}

#[test]
fn freeing_null_is_a_no_op() {
    unsafe { bc_string_free(std::ptr::null_mut()) };
}
