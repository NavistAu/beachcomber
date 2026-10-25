//! Client lifecycle: `bc_version`, `bc_client_new`, `bc_client_free`. Task
//! 3.3 of docs/superpowers/plans/2026-08-15-client-abi-and-sdk-refactor.md.

use std::ffi::{CStr, CString};
use std::ptr;

use beachcomber::{bc_client_free, bc_client_new, bc_version};

#[test]
fn version_matches_beachcomber_version() {
    let ptr = bc_version();
    assert!(!ptr.is_null());
    let s = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap();
    assert_eq!(s, libbeachcomber::VERSION);
}

#[test]
fn client_new_with_null_options_succeeds() {
    let client = unsafe { bc_client_new(ptr::null()) };
    assert!(!client.is_null());
    unsafe { bc_client_free(client) };
}

#[test]
fn client_new_with_malformed_json_still_returns_a_handle() {
    let json = CString::new("{ this is not valid json").unwrap();
    let client = unsafe { bc_client_new(json.as_ptr()) };
    assert!(
        !client.is_null(),
        "malformed options_json must still yield a handle; the error defers to the first op"
    );
    unsafe { bc_client_free(client) };
}

#[test]
fn client_new_with_well_formed_options_succeeds() {
    let json = CString::new(
        r#"{"socket_path":"/tmp/does-not-matter.sock","timeout_ms":50,"autostart":false}"#,
    )
    .unwrap();
    let client = unsafe { bc_client_new(json.as_ptr()) };
    assert!(!client.is_null());
    unsafe { bc_client_free(client) };
}

#[test]
fn client_free_null_is_a_no_op() {
    unsafe { bc_client_free(ptr::null_mut()) };
}

/// `bc_socket_path` is the socket a client with no `socket_path` option
/// connects to: `$BEACHCOMBER_SOCKET` when set, else the per-user default.
#[test]
fn socket_path_is_the_one_resolution() {
    let read = || {
        let ptr = beachcomber::bc_socket_path();
        let body = unsafe { std::ffi::CStr::from_ptr(ptr) }
            .to_str()
            .unwrap()
            .to_string();
        unsafe { beachcomber::envelope::bc_string_free(ptr) };
        serde_json::from_str::<serde_json::Value>(&body).unwrap()
    };
    // SAFETY: nextest runs every test in its own process.
    unsafe { std::env::set_var("BEACHCOMBER_SOCKET", "/env/sock") };
    assert_eq!(read()["data"], serde_json::json!("/env/sock"));
    unsafe { std::env::remove_var("BEACHCOMBER_SOCKET") };
    let default = libbeachcomber::socket::default_path();
    assert_eq!(read()["data"], serde_json::json!(default.to_string_lossy()));
}
