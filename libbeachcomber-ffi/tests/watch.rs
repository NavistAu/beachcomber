//! Watch: `bc_watch_open`, `bc_watch_next`, `bc_watch_cancel`,
//! `bc_watch_free`. Task 3.7 of
//! docs/superpowers/plans/2026-08-15-client-abi-and-sdk-refactor.md.
//!
//! Runs against a real in-process daemon spawned by `tests/common/daemon.rs`.

mod common;
use common::daemon::DaemonGuard;

use std::ffi::{CStr, CString, c_char};
use std::path::Path;
use std::ptr;
use std::sync::Barrier;
use std::sync::atomic::{AtomicUsize, Ordering};

use beachcomber::envelope::bc_string_free;
use beachcomber::{
    BcClient, bc_client_free, bc_client_new, bc_put, bc_watch_cancel, bc_watch_free, bc_watch_next,
    bc_watch_open,
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
    assert!(!ptr.is_null(), "call_watch_next must never return NULL");
    let body = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_string();
    unsafe { bc_string_free(ptr) };
    serde_json::from_str(&body).expect("envelope must be valid JSON")
}

/// Seeds a virtual provider `key` with `data` (a JSON object literal, e.g.
/// `r#"{"field":"v1"}"#`). Virtual providers have no scheduler or poller
/// attached — nothing updates them spontaneously — so a watch on one only
/// ever emits an event when this (or another) `put` targets it, which is
/// what makes the timeout/eof/cancel tests below deterministic: unlike a
/// real provider such as `hostname`, there is no risk of an unplanned
/// second broadcast racing the assertion.
fn put_virtual(client: *mut BcClient, key: &str, data: &str) {
    let v = read(unsafe {
        bc_put(
            client,
            cs(key).as_ptr(),
            cs(data).as_ptr(),
            ptr::null(),
            ptr::null(),
        )
    });
    assert_eq!(v["ok"], serde_json::json!(true), "put must succeed: {v}");
}

/// Wraps a raw pointer so it can be shared across threads in these tests.
/// Sound here because the only cross-thread call this crate documents is
/// exactly what the busy/cancel tests exercise: concurrent `bc_watch_next`,
/// and `bc_watch_cancel` from a different thread than the one blocked in
/// `bc_watch_next`.
struct SendPtr<T>(*mut T);
unsafe impl<T> Send for SendPtr<T> {}
unsafe impl<T> Sync for SendPtr<T> {}

#[test]
fn watch_next_first_call_yields_an_event_outcome() {
    // The daemon's first watch frame is always the current value, so the
    // very first `bc_watch_next` call — even with a generous wait — proves
    // the "event" outcome without needing to trigger a change.
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let key = cs("hostname.short");
    let watch = unsafe { bc_watch_open(client, key.as_ptr(), ptr::null()) };
    assert!(!watch.is_null());

    let v = read(unsafe { bc_watch_next(watch, 2000) });
    assert_eq!(v["ok"], serde_json::json!(true));
    assert_eq!(v["outcome"], serde_json::json!("event"));
    assert!(v["data"].is_object());

    unsafe { bc_watch_free(watch) };
    unsafe { bc_client_free(client) };
}

#[test]
fn watch_next_zero_timeout_polls_and_times_out_when_nothing_is_ready() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    put_virtual(client, "watch_timeout_test", r#"{"field":"v1"}"#);
    let key = cs("watch_timeout_test.field");
    let watch = unsafe { bc_watch_open(client, key.as_ptr(), ptr::null()) };
    assert!(!watch.is_null());

    // Consume the initial-value event first so the next poll has nothing
    // pending.
    let first = read(unsafe { bc_watch_next(watch, 2000) });
    assert_eq!(first["outcome"], serde_json::json!("event"));

    let v = read(unsafe { bc_watch_next(watch, 0) });
    assert_eq!(v["ok"], serde_json::json!(true));
    assert_eq!(v["outcome"], serde_json::json!("timeout"));

    unsafe { bc_watch_free(watch) };
    unsafe { bc_client_free(client) };
}

#[test]
fn watch_next_reports_eof_after_the_daemon_closes_the_connection() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    put_virtual(client, "watch_eof_test", r#"{"field":"v1"}"#);
    let key = cs("watch_eof_test.field");
    let watch = unsafe { bc_watch_open(client, key.as_ptr(), ptr::null()) };
    assert!(!watch.is_null());

    let first = read(unsafe { bc_watch_next(watch, 2000) });
    assert_eq!(first["outcome"], serde_json::json!("event"));

    // Shutting down the daemon closes the watch's socket out from under it.
    drop(daemon);

    let v = read(unsafe { bc_watch_next(watch, 5000) });
    assert_eq!(v["ok"], serde_json::json!(true));
    assert_eq!(v["outcome"], serde_json::json!("eof"));

    unsafe { bc_watch_free(watch) };
    unsafe { bc_client_free(client) };
}

#[test]
fn watch_next_on_an_invalid_key_yields_an_error_envelope() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    // A dotted sub-path whose head exists but whose tail doesn't is the
    // documented "unknown field" error (mirrors `get`'s own semantics; see
    // tests/conformance/put/nested_object_data.json), so it errors as the
    // stream's very first frame rather than the ordinary miss a wholly
    // unregistered provider would produce.
    put_virtual(
        client,
        "watch_error_test",
        r#"{"widget":{"kind":"renderable"}}"#,
    );
    let key = cs("watch_error_test.widget.nonexistent");
    let watch = unsafe { bc_watch_open(client, key.as_ptr(), ptr::null()) };
    assert!(!watch.is_null());

    let v = read(unsafe { bc_watch_next(watch, 2000) });
    assert_eq!(v["ok"], serde_json::json!(false));
    assert!(v["error"]["kind"].is_string());
    assert_ne!(v["error"]["kind"], serde_json::json!("busy"));

    unsafe { bc_watch_free(watch) };
    unsafe { bc_client_free(client) };
}

#[test]
fn watch_cancel_from_another_thread_unblocks_a_pending_next() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    put_virtual(client, "watch_cancel_test", r#"{"field":"v1"}"#);
    let key = cs("watch_cancel_test.field");
    let watch = unsafe { bc_watch_open(client, key.as_ptr(), ptr::null()) };
    assert!(!watch.is_null());

    // Consume the initial-value event so the next call genuinely blocks
    // waiting for a change that will never come.
    let first = read(unsafe { bc_watch_next(watch, 2000) });
    assert_eq!(first["outcome"], serde_json::json!("event"));

    let watch_ptr = SendPtr(watch);
    let watch_ptr = &watch_ptr;
    let start = std::time::Instant::now();
    std::thread::scope(|scope| {
        let blocked = scope.spawn(|| read(unsafe { bc_watch_next(watch_ptr.0, -1) }));

        std::thread::sleep(std::time::Duration::from_millis(75));
        unsafe { bc_watch_cancel(watch_ptr.0) };

        let v = blocked.join().unwrap();
        assert_eq!(v["ok"], serde_json::json!(true));
        assert_eq!(v["outcome"], serde_json::json!("cancelled"));
    });
    assert!(
        start.elapsed() < std::time::Duration::from_secs(2),
        "cancel must unblock a pending -1 (block indefinitely) call promptly"
    );

    unsafe { bc_watch_free(watch) };
    unsafe { bc_client_free(client) };
}

#[test]
fn watch_next_called_concurrently_from_two_threads_yields_busy_for_one() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let key = cs("hostname.short");
    let watch = unsafe { bc_watch_open(client, key.as_ptr(), ptr::null()) };
    assert!(!watch.is_null());

    let watch_ptr = SendPtr(watch);
    let ok_count = AtomicUsize::new(0);
    let busy_count = AtomicUsize::new(0);
    let threads = 8;
    let iters = 25;
    let barrier = Barrier::new(threads);

    std::thread::scope(|scope| {
        for _ in 0..threads {
            let watch_ptr = &watch_ptr;
            let ok_count = &ok_count;
            let busy_count = &busy_count;
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                for _ in 0..iters {
                    // timeout_ms = 0: whichever thread wins the lock returns
                    // promptly (event or timeout), so this doesn't hang on
                    // the one thread that does win it.
                    let v = read(unsafe { bc_watch_next(watch_ptr.0, 0) });
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
        "at least one concurrent caller must win the lock"
    );
    assert!(
        busy_count.load(Ordering::SeqCst) > 0,
        "at least one concurrent caller must observe busy rather than interleave on the socket"
    );

    unsafe { bc_watch_free(watch) };
    unsafe { bc_client_free(client) };
}
