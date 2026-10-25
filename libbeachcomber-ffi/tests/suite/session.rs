//! Sessions: `bc_session_open`, `bc_session_get`, `bc_session_put`,
//! `bc_session_refresh`, `bc_session_set_context`, `bc_session_close`. Task 3.6 of
//! docs/superpowers/plans/2026-08-15-client-abi-and-sdk-refactor.md.
//!
//! Runs against a real in-process daemon spawned by `tests/suite/common/daemon.rs`.

use crate::common::daemon::DaemonGuard;

use std::ffi::{CStr, CString, c_char};
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Barrier, Mutex};

use beachcomber::envelope::bc_string_free;
use beachcomber::{
    BcClient, bc_client_free, bc_client_new, bc_get, bc_session_close, bc_session_get,
    bc_session_open, bc_session_put, bc_session_refresh, bc_session_set_context,
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

/// Wraps a raw pointer so it can be shared across threads in these tests.
/// Sound here because every operation this crate exposes on the pointee
/// (`BcSession`/`BcClient`) takes `&self`/an internal lock and is documented
/// as safe for concurrent, cross-thread use.
struct SendPtr<T>(*mut T);
unsafe impl<T> Send for SendPtr<T> {}
unsafe impl<T> Sync for SendPtr<T> {}

#[test]
fn two_thread_session_yields_busy_for_one_and_leaves_the_stream_intact() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let session = unsafe { bc_session_open(client) };
    assert!(!session.is_null());

    let session_ptr = SendPtr(session);
    let ok_count = AtomicUsize::new(0);
    let busy_count = AtomicUsize::new(0);
    let threads = 8;
    let iters = 25;
    let barrier = Barrier::new(threads);

    std::thread::scope(|scope| {
        for _ in 0..threads {
            let session_ptr = &session_ptr;
            let ok_count = &ok_count;
            let busy_count = &busy_count;
            let barrier = &barrier;
            scope.spawn(move || {
                let key = cs("hostname.short");
                barrier.wait();
                for _ in 0..iters {
                    let v = read(unsafe {
                        bc_session_get(session_ptr.0, key.as_ptr(), ptr::null(), 0)
                    });
                    if v["ok"] == serde_json::json!(true) {
                        ok_count.fetch_add(1, Ordering::SeqCst);
                    } else if v["error"]["kind"] == serde_json::json!("busy") {
                        busy_count.fetch_add(1, Ordering::SeqCst);
                    } else {
                        panic!("unexpected envelope: {v}");
                    }
                }
            });
        }
    });

    assert!(
        ok_count.load(Ordering::SeqCst) > 0,
        "at least one concurrent caller must succeed"
    );
    assert!(
        busy_count.load(Ordering::SeqCst) > 0,
        "at least one concurrent caller must observe busy rather than interleave on the socket"
    );

    // The stream must still work after concurrent contention.
    let key = cs("hostname.short");
    let v = read(unsafe { bc_session_get(session, key.as_ptr(), ptr::null(), 0) });
    assert_eq!(
        v["ok"],
        serde_json::json!(true),
        "stream must remain intact: {v}"
    );

    unsafe { bc_session_close(session) };
    unsafe { bc_client_free(client) };
}

#[test]
fn session_open_on_unreachable_daemon_defers_error_to_first_op() {
    let sock = Path::new("/tmp/beachcomber-ffi-session-test-unreachable.sock");
    let _ = std::fs::remove_file(sock);
    let client = client_for(sock);

    let session = unsafe { bc_session_open(client) };
    assert!(
        !session.is_null(),
        "bc_session_open must not return NULL for an unreachable daemon"
    );

    let key = cs("hostname.short");
    let v = read(unsafe { bc_session_get(session, key.as_ptr(), ptr::null(), 0) });
    assert_eq!(v["ok"], serde_json::json!(false));
    assert_ne!(v["error"]["kind"], serde_json::json!("busy"));

    unsafe { bc_session_close(session) };
    unsafe { bc_client_free(client) };
}

#[test]
fn bc_client_is_send_and_sync_under_concurrent_ops_from_several_threads() {
    use beachcomber::bc_get;

    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let client_ptr = SendPtr(client);
    let ok_count = AtomicUsize::new(0);
    let threads = 8;
    let iters = 25;
    let barrier = Barrier::new(threads);
    // Serialises assertion panics inside scoped threads only for clarity of
    // failure messages; not part of the mechanism under test.
    let failures: Mutex<Vec<String>> = Mutex::new(Vec::new());

    std::thread::scope(|scope| {
        for _ in 0..threads {
            let client_ptr = &client_ptr;
            let ok_count = &ok_count;
            let barrier = &barrier;
            let failures = &failures;
            scope.spawn(move || {
                let key = cs("hostname.short");
                barrier.wait();
                for _ in 0..iters {
                    let v = read(unsafe { bc_get(client_ptr.0, key.as_ptr(), ptr::null(), 0) });
                    if v["ok"] == serde_json::json!(true) {
                        ok_count.fetch_add(1, Ordering::SeqCst);
                    } else {
                        failures.lock().unwrap().push(v.to_string());
                    }
                }
            });
        }
    });

    let failures = failures.into_inner().unwrap();
    assert!(failures.is_empty(), "unexpected failures: {failures:?}");
    assert_eq!(ok_count.load(Ordering::SeqCst), threads * iters);

    unsafe { bc_client_free(client) };
}

/// A session's directory is its own: `bc_session_set_context` reaches the
/// daemon with every NULL-path `bc_session_put`, `bc_session_get` and
/// `bc_session_refresh`, and a put lands there, not at the global entry.
#[test]
fn a_sessions_directory_is_sent_with_put_get_and_refresh() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let session = unsafe { bc_session_open(client) };
    let dir = cs("/tmp");

    let v = read(unsafe { bc_session_set_context(session, dir.as_ptr()) });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    let key = cs("sessctx");
    let data = cs(r#"{"v":"here"}"#);
    let v = read(unsafe {
        bc_session_put(
            session,
            key.as_ptr(),
            data.as_ptr(),
            ptr::null(),
            ptr::null(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");

    let leaf = cs("sessctx.v");
    let v = read(unsafe { bc_session_get(session, leaf.as_ptr(), ptr::null(), 0) });
    assert_eq!(v["data"]["data"], serde_json::json!("here"), "{v}");
    let v = read(unsafe { bc_get(client, leaf.as_ptr(), dir.as_ptr(), 0) });
    assert_eq!(v["data"]["data"], serde_json::json!("here"), "at /tmp: {v}");
    let v = read(unsafe { bc_get(client, leaf.as_ptr(), ptr::null(), 0) });
    assert!(v["data"]["data"].is_null(), "not global: {v}");

    // A refresh of a namespace with no Sources is the daemon's refusal: it
    // reached the daemon through the session.
    let v = read(unsafe { bc_session_refresh(session, key.as_ptr(), ptr::null()) });
    assert_eq!(v["ok"], serde_json::json!(false), "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("nothing to refresh")),
        "{v}"
    );

    unsafe { bc_session_close(session) };
    unsafe { bc_client_free(client) };
}
