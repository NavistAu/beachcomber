//! `exec()` through the ABI is opt-in per client (design §5.3, outcome 26).
//! The CLI's user already has a shell, so `comb eval`/`comb get` always
//! allow it; an embedding caller does not get to spawn processes on the
//! user's behalf unless it says so — `bc_client_new("{\"exec\":true}")`.
//! Without the option an `exec()` call in an evaluated expression fails with
//! the on-topic disabled-exec message rather than running anything.
//!
//! None of these sources references a daemon-backed ref, so no live daemon
//! is needed: an unstartable socket with `autostart:false` means any daemon
//! contact would surface as a distinct transport failure, not the messages
//! asserted here.

use std::ffi::{CStr, CString, c_char};
use std::path::Path;
use std::ptr;

use beachcomber::envelope::bc_string_free;
use beachcomber::{BcClient, bc_client_free, bc_client_new, bc_eval};

fn client_for(sock: &Path, exec: bool) -> *mut BcClient {
    let json = format!(
        r#"{{"socket_path":"{}","autostart":false,"timeout_ms":2000,"exec":{exec}}}"#,
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

fn eval(client: *mut BcClient, src: &str) -> serde_json::Value {
    let src = cs(src);
    let cwd = cs("/tmp");
    read(unsafe { bc_eval(client, src.as_ptr(), cwd.as_ptr(), ptr::null()) })
}

/// The default client: `exec()` is registered but disabled, and a call fails
/// at the call site with the disabled message — a `server_error`, the
/// resolver's runtime failure — rather than a generic unknown-function error.
#[test]
fn bc_eval_of_exec_call_returns_disabled_message_without_the_option() {
    let dir = tempfile::TempDir::new().unwrap();
    let client = client_for(&dir.path().join("nonexistent.sock"), false);

    let v = eval(client, "{{ exec('printf', 'x') }}");
    assert_eq!(v["ok"], serde_json::json!(false), "{v}");
    assert_eq!(v["error"]["kind"], serde_json::json!("server_error"), "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("exec() is disabled in this context"),
        "unexpected error: {v}"
    );

    unsafe { bc_client_free(client) };
}

/// `{"exec":true}` on `bc_client_new` reaches the resolver: the same call
/// runs and yields the command's stdout.
#[test]
fn bc_eval_of_exec_call_runs_with_the_option() {
    let dir = tempfile::TempDir::new().unwrap();
    let client = client_for(&dir.path().join("nonexistent.sock"), true);

    let v = eval(client, "{{ exec('printf', 'x') }}");
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["data"], serde_json::json!("x"));

    unsafe { bc_client_free(client) };
}

/// A command that fails is a render-time failure — `server_error`, the
/// moment's fault — not `parse_error`, which is reserved for a source that
/// does not compile.
#[test]
fn bc_eval_of_a_failing_exec_is_a_server_error() {
    let dir = tempfile::TempDir::new().unwrap();
    let client = client_for(&dir.path().join("nonexistent.sock"), true);

    let v = eval(client, "{{ exec('false') }}");
    assert_eq!(v["ok"], serde_json::json!(false), "{v}");
    assert_eq!(v["error"]["kind"], serde_json::json!("server_error"), "{v}");

    unsafe { bc_client_free(client) };
}
