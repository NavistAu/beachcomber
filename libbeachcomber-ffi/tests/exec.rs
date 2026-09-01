//! `bc_eval` stays exec-off: Task 5 of the exec() plan enables `.with_exec()`
//! on the CLI's user-initiated evaluation (`comb eval`/`comb get`) but the
//! FFI/SDK surface must not opt in — the design principle is that the CLI
//! user already has a shell, while an embedded/FFI caller does not get to
//! spawn processes on the caller's behalf. `bc_eval` builds its
//! `EvalContext` with plain `EvalContext::new(...)`, so an `exec()` call in
//! an evaluated expression must fail with the on-topic disabled-exec
//! message rather than actually running anything.

use std::ffi::{CStr, CString, c_char};
use std::path::Path;
use std::ptr;

use beachcomber::envelope::bc_string_free;
use beachcomber::{BcClient, bc_client_free, bc_client_new, bc_eval};

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

/// `exec("sh", "-c", "echo hi")` references no daemon-backed refs, so no
/// live daemon is needed to prove this: an unstartable socket with
/// `autostart:false` means any daemon contact would surface as a distinct
/// transport failure, not the disabled-exec message asserted here.
#[test]
fn bc_eval_of_exec_call_returns_disabled_message() {
    let dir = tempfile::TempDir::new().unwrap();
    let bogus_sock = dir.path().join("nonexistent.sock");
    let client = client_for(&bogus_sock);
    let cwd = cs("/tmp");
    let template = cs(r#"{{ exec("sh", "-c", "echo hi") }}"#);

    let v = read(unsafe {
        bc_eval(
            client,
            template.as_ptr(),
            cwd.as_ptr(),
            ptr::null(),
            ptr::null(),
        )
    });

    assert_eq!(v["ok"], serde_json::json!(false), "{v}");
    let message = v["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("exec() is disabled in this context"),
        "unexpected error: {v}"
    );

    unsafe { bc_client_free(client) };
}
