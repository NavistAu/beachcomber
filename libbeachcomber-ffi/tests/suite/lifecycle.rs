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

/// The envelope of a client's first op (`bc_hello`), made with `options`,
/// and how long it took.
fn first_op(options: &str) -> (serde_json::Value, std::time::Duration) {
    let json = CString::new(options).unwrap();
    let client = unsafe { bc_client_new(json.as_ptr()) };
    let started = std::time::Instant::now();
    let ptr = unsafe { beachcomber::bc_hello(client) };
    let took = started.elapsed();
    let body = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_string();
    unsafe { beachcomber::envelope::bc_string_free(ptr) };
    unsafe { bc_client_free(client) };
    (serde_json::from_str(&body).unwrap(), took)
}

/// `connect_timeout_ms` is how long connecting keeps retrying a socket nobody
/// serves: 0 is one attempt, and a budget replaces the 1500ms default.
#[test]
fn connect_timeout_ms_bounds_the_connect() {
    let dir = tempfile::Builder::new()
        .prefix("bcct")
        .tempdir_in("/tmp")
        .unwrap();
    let sock = dir.path().join("sock");
    let options = |ms: u64| {
        format!(
            r#"{{"socket_path":"{}","autostart":false,"connect_timeout_ms":{ms}}}"#,
            sock.display()
        )
    };

    let (v, took) = first_op(&options(0));
    assert_eq!(v["error"]["kind"], "connection_failed", "{v}");
    assert!(
        took < std::time::Duration::from_millis(250),
        "one attempt took {took:?}"
    );

    let (v, took) = first_op(&options(50));
    assert_eq!(v["error"]["kind"], "connection_failed", "{v}");
    assert!(
        took >= std::time::Duration::from_millis(50),
        "gave up after {took:?}"
    );
    assert!(
        took < std::time::Duration::from_millis(1200),
        "retried for {took:?}"
    );
}

/// An option `bc_client_new` does not take is refused on the handle's first
/// op, not ignored: each option has one spelling.
#[test]
fn an_unknown_option_is_refused() {
    let (v, _) = first_op(r#"{"socket_path":"/tmp/does-not-matter.sock","timeout":5}"#);
    assert_eq!(v["error"]["kind"], "parse_error", "{v}");
    let message = v["error"]["message"].as_str().unwrap();
    assert!(message.contains("unknown field `timeout`"), "{message}");
}

/// A `timeout_ms` of 0 would fail every read and write; it is refused when
/// the client is made, naming the option.
#[test]
fn a_zero_timeout_ms_is_refused() {
    let (v, _) = first_op(r#"{"socket_path":"/tmp/does-not-matter.sock","timeout_ms":0}"#);
    assert_eq!(v["error"]["kind"], "parse_error", "{v}");
    let message = v["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("timeout_ms must be at least 1"),
        "{message}"
    );
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
