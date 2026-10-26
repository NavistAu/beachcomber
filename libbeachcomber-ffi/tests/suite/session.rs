//! Sessions: `bc_session_open`, `bc_session_get`, `bc_session_put`,
//! `bc_session_refresh`, `bc_session_set_context`, `bc_session_close`. Task 3.6 of
//! docs/superpowers/plans/2026-08-15-client-abi-and-sdk-refactor.md.
//!
//! Runs against a real in-process daemon spawned by `tests/suite/common/daemon.rs`.

use crate::common::client::{Options, client_for, client_with, cs, read};
use crate::common::daemon::DaemonGuard;

use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Barrier, Mutex};

use beachcomber::{
    bc_client_free, bc_client_new, bc_get, bc_session_close, bc_session_get, bc_session_open,
    bc_session_put, bc_session_refresh, bc_session_set_context,
};

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
                        bc_session_get(
                            session_ptr.0,
                            key.as_ptr(),
                            ptr::null(),
                            ptr::null(),
                            ptr::null(),
                        )
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
    let v = read(unsafe {
        bc_session_get(session, key.as_ptr(), ptr::null(), ptr::null(), ptr::null())
    });
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
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = dir.path().join("nothing-listens-here.sock");
    let client = client_with(
        &sock,
        Options {
            connect_timeout_ms: Some(0),
            ..Options::default()
        },
    );

    let session = unsafe { bc_session_open(client) };
    assert!(
        !session.is_null(),
        "bc_session_open must not return NULL for an unreachable daemon"
    );

    let key = cs("hostname.short");
    let v = read(unsafe {
        bc_session_get(session, key.as_ptr(), ptr::null(), ptr::null(), ptr::null())
    });
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
                    let v = read(unsafe {
                        bc_get(
                            client_ptr.0,
                            key.as_ptr(),
                            ptr::null(),
                            ptr::null(),
                            ptr::null(),
                        )
                    });
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
    let v = read(unsafe {
        bc_session_get(
            session,
            leaf.as_ptr(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
        )
    });
    assert_eq!(v["data"], serde_json::json!("here"), "{v}");
    let v = read(unsafe {
        bc_get(
            client,
            leaf.as_ptr(),
            dir.as_ptr(),
            ptr::null(),
            ptr::null(),
        )
    });
    assert_eq!(v["data"], serde_json::json!("here"), "at /tmp: {v}");
    let v = read(unsafe { bc_get(client, leaf.as_ptr(), ptr::null(), ptr::null(), ptr::null()) });
    assert!(v["data"].is_null(), "not global: {v}");

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

/// A daemon that does not answer within `timeout_ms` is `kind: "timeout"`
/// on a session as on a client, for a read and a write alike.
#[test]
fn a_slow_session_get_and_a_slow_put_report_timeout() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("sock");
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            held.push(stream);
        }
    });
    let options = cs(&format!(
        r#"{{"socket_path":"{}","autostart":false,"timeout_ms":5}}"#,
        sock.display()
    ));
    let client = unsafe { bc_client_new(options.as_ptr()) };

    let session = unsafe { bc_session_open(client) };
    let get = read(unsafe {
        bc_session_get(
            session,
            cs("p.x").as_ptr(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
        )
    });
    assert_eq!(get["error"]["kind"], "timeout", "{get}");

    let put = read(unsafe {
        beachcomber::bc_put(
            client,
            cs("p").as_ptr(),
            cs(r#"{"x":1}"#).as_ptr(),
            ptr::null(),
            ptr::null(),
        )
    });
    assert_eq!(put["error"]["kind"], "timeout", "{put}");

    unsafe { bc_session_close(session) };
    unsafe { bc_client_free(client) };
}

/// `bc_session_get` is `bc_get` on the session's connection: a template
/// renders with the call's environment, at the session's directory.
#[test]
fn a_session_get_renders_at_the_sessions_directory() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let session = unsafe { bc_session_open(client) };
    let put = |key: &str, data: &str, path: Option<&str>| {
        let (key, data, path) = (cs(key), cs(data), path.map(cs));
        let v = read(unsafe {
            bc_session_put(
                session,
                key.as_ptr(),
                data.as_ptr(),
                ptr::null(),
                path.as_ref().map_or(ptr::null(), |p| p.as_ptr()),
            )
        });
        assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    };
    put(
        "rs:template",
        r#"{"t":"{{ env.WHO }}@{{ rs.where }}"}"#,
        None,
    );
    put("rs", r#"{"where":"global"}"#, None);
    put("rs", r#"{"where":"tmp"}"#, Some("/tmp"));

    let key = cs("rs.t");
    let env = cs(r#"{"WHO":"me"}"#);
    let v = read(unsafe {
        bc_session_get(
            session,
            key.as_ptr(),
            ptr::null(),
            env.as_ptr(),
            ptr::null(),
        )
    });
    assert_eq!(v["data"], serde_json::json!("me@global"), "{v}");

    let dir = cs("/tmp");
    read(unsafe { bc_session_set_context(session, dir.as_ptr()) });
    let v = read(unsafe {
        bc_session_get(
            session,
            key.as_ptr(),
            ptr::null(),
            env.as_ptr(),
            ptr::null(),
        )
    });
    assert_eq!(v["data"], serde_json::json!("me@tmp"), "{v}");

    unsafe { bc_session_close(session) };
    unsafe { bc_client_free(client) };
}
