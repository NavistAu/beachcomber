//! Watch: `bc_watch_open`, `bc_watch_next`, `bc_watch_cancel`,
//! `bc_watch_free`.
//!
//! Since Plan 4 Task 3 a watch is a *resolved* watch: `bc_watch_open` takes
//! the same `cwd`/`env_json` pair `bc_get` does, every event is the
//! value `bc_get` would return for that key, and the fan-in underneath
//! subscribes to every key the resolution read plus the provider's
//! `<p>:template`, so editing a template the value is rendered from fires
//! the watch.
//!
//! Runs against a real in-process daemon spawned by `tests/suite/common/daemon.rs`.

use crate::common::client::{Options, client_for, client_with, cs, read};
use crate::common::daemon::DaemonGuard;
use crate::common::socket::IsolatedSocket;

use std::ffi::CString;
use std::ptr;

use beachcomber::{
    BcClient, BcWatch, bc_client_free, bc_get, bc_put, bc_watch_cancel, bc_watch_free,
    bc_watch_next, bc_watch_open,
};

/// The directory every watch here addresses. Nothing is stored at it: each
/// fixture `put`s globally, and the global entry is the last link of every
/// path's chain (canon `field_resolution.md` §"Path resolution").
const CWD: &str = "/tmp";

/// `bc_watch_open` at [`CWD`] with an optional environment.
fn open_watch(client: *mut BcClient, key: &str, env: Option<&str>) -> *mut BcWatch {
    let key = cs(key);
    let cwd = cs(CWD);
    let env = env.map(cs);
    let w = unsafe {
        bc_watch_open(
            client,
            key.as_ptr(),
            cwd.as_ptr(),
            env.as_ref().map_or(ptr::null(), |e| e.as_ptr()),
        )
    };
    assert!(!w.is_null());
    w
}

/// Seeds a static namespace `key` with `data` (a JSON object literal, e.g.
/// `r#"{"field":"v1"}"#`). A namespace holding only static records has no
/// Source behind it, so no scheduler or poller updates it spontaneously — a
/// watch on one only ever emits an event when this (or another) `put` targets
/// it, which is what makes the timeout/eof/cancel tests below deterministic:
/// unlike a real provider such as `hostname`, there is no risk of an unplanned
/// second broadcast racing the assertion.
fn put_static(client: *mut BcClient, key: &str, data: &str) {
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

/// A watch opened with no `env_json` renders with this process's environment,
/// as `bc_get` does.
#[test]
fn a_watch_with_no_env_reads_the_process_home() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    put_static(client, "watchenv:template", r#"{"home":"{{ env.HOME }}"}"#);
    let watch = open_watch(client, "watchenv.home", None);

    let v = read(unsafe { bc_watch_next(watch, 2000) });
    assert_eq!(
        v["data"],
        serde_json::json!(std::env::var("HOME").unwrap()),
        "{v}"
    );

    unsafe { bc_watch_free(watch) };
    unsafe { bc_client_free(client) };
}

/// The entire point of `open_owned`/`Arc<Client>`: a `BcWatch` holds its own
/// share of the client, so freeing the client handle does not invalidate a
/// watch still in use — free order between the two is independent. The
/// pending-initial answer is spent before the free (it is served from the
/// watch's own cached value and touches no client), so the post-free frame
/// must re-resolve through the shared client — a read of freed heap if that
/// share were a borrow.
#[test]
fn watch_next_works_after_the_client_that_opened_it_is_freed() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let writer = client_for(&daemon.path);
    put_static(client, "watch_free_order_test", r#"{"field":"v1"}"#);
    let watch = open_watch(client, "watch_free_order_test.field", None);

    let first = read(unsafe { bc_watch_next(watch, 2000) });
    assert_eq!(first["data"], serde_json::json!("v1"), "{first}");

    unsafe { bc_client_free(client) };

    // A put from any connection fires the watch — the daemon broadcasts per
    // (provider, path), not per connection — and the frame forces a
    // re-resolve through the client the watch holds a share of.
    put_static(writer, "watch_free_order_test", r#"{"field":"v2"}"#);
    let v = read(unsafe { bc_watch_next(watch, 5000) });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["outcome"], serde_json::json!("event"), "{v}");
    assert_eq!(v["data"], serde_json::json!("v2"), "{v}");

    unsafe { bc_watch_free(watch) };
    unsafe { bc_client_free(writer) };
}

/// The equivalence Plan 3 models the whole surface on: a watch's first event
/// is exactly what `bc_get` returns for the same key, `cwd` and
/// `env_json` — template rendered, environment applied.
#[test]
fn watch_first_event_equals_bc_get_of_the_same_key() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let env = r#"{"EQ_VALUE":"rendered"}"#;
    put_static(client, "eqprov:template", r#"{"f":"{{ env.EQ_VALUE }}"}"#);

    let resolved = read(unsafe {
        bc_get(
            client,
            cs("eqprov.f").as_ptr(),
            cs(CWD).as_ptr(),
            cs(env).as_ptr(),
            ptr::null(),
        )
    });
    assert_eq!(resolved["ok"], serde_json::json!(true), "{resolved}");
    assert_eq!(resolved["data"], serde_json::json!("rendered"));

    let watch = open_watch(client, "eqprov.f", Some(env));
    let v = read(unsafe { bc_watch_next(watch, 2000) });
    assert_eq!(v["outcome"], serde_json::json!("event"), "{v}");
    assert_eq!(
        v["data"], resolved["data"],
        "the first event must be bc_get's answer"
    );

    unsafe { bc_watch_free(watch) };
    unsafe { bc_client_free(client) };
}

#[test]
fn watch_next_zero_timeout_polls_and_times_out_when_nothing_is_ready() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    put_static(client, "watch_timeout_test", r#"{"field":"v1"}"#);
    let watch = open_watch(client, "watch_timeout_test.field", None);

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
    put_static(client, "watch_eof_test", r#"{"field":"v1"}"#);
    let watch = open_watch(client, "watch_eof_test.field", None);

    let first = read(unsafe { bc_watch_next(watch, 2000) });
    assert_eq!(first["outcome"], serde_json::json!("event"));

    // Shutting down the daemon closes the watch's socket out from under it.
    drop(daemon);

    let v = read(unsafe { bc_watch_next(watch, 5000) });
    assert_eq!(v["ok"], serde_json::json!(true));
    assert_eq!(v["outcome"], serde_json::json!("eof"));

    // The ended state is latched: a watch that has reported `eof` reports it
    // again rather than resuming a poll over streams that can never answer.
    let again = read(unsafe { bc_watch_next(watch, 0) });
    assert_eq!(again["outcome"], serde_json::json!("eof"), "{again}");

    unsafe { bc_watch_free(watch) };
    unsafe { bc_client_free(client) };
}

#[test]
fn watch_next_on_an_invalid_key_yields_an_error_envelope() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    // An invalid facet key (two colons) is refused by the daemon on `watch`
    // exactly as on `get`. A facet key has no template half to render, so the
    // refusal arrives as the stream's first frame — on the first
    // `bc_watch_next`, not at open.
    let watch = open_watch(client, "a:b:c.x", None);

    let v = read(unsafe { bc_watch_next(watch, 2000) });
    assert_eq!(v["ok"], serde_json::json!(false));
    assert!(v["error"]["kind"].is_string());
    assert_ne!(v["error"]["kind"], serde_json::json!("busy"));

    // Latched: the same error comes back rather than the watch resuming.
    let again = read(unsafe { bc_watch_next(watch, 0) });
    assert_eq!(again["error"], v["error"], "{again}");

    unsafe { bc_watch_free(watch) };
    unsafe { bc_client_free(client) };
}

/// A refusal the daemon has already sent, and closed the connection after, is
/// still the answer: a frame read off the wire comes before the end of the
/// stream. The daemon here is a stand-in that answers the watch with its
/// refusal and hangs up; it has finished, and the connection is closed, before
/// `bc_watch_next` is called. That is the order a loaded machine produces
/// when the caller is slower than the daemon, so it is the case a watch must
/// not turn into `eof`. The refusal's version-skew probe finds nobody serving
/// the socket by then, and tries it once.
#[test]
fn a_refusal_sent_before_the_connection_closed_is_still_the_answer() {
    let socket = IsolatedSocket::new();
    let listener = std::os::unix::net::UnixListener::bind(&socket.path).expect("bind");
    let client = client_with(
        &socket.path,
        Options {
            connect_timeout_ms: Some(0),
            ..Options::default()
        },
    );

    let daemon = std::thread::spawn(move || {
        use std::io::{BufRead, Write};
        let (stream, _) = listener.accept().expect("accept");
        let mut request = String::new();
        std::io::BufReader::new(&stream)
            .read_line(&mut request)
            .expect("the watch request");
        (&stream)
            .write_all(b"{\"ok\":false,\"error\":\"invalid facet key: a:b:c\"}\n")
            .expect("the refusal");
        request
    });
    let watch = open_watch(client, "a:b:c.x", None);
    let request = daemon.join().expect("the stand-in daemon");
    assert!(request.contains("\"watch\""), "{request}");

    let v = read(unsafe { bc_watch_next(watch, 2000) });
    assert_eq!(v["ok"], serde_json::json!(false), "{v}");
    assert_eq!(v["error"]["kind"], serde_json::json!("server_error"), "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("invalid facet key: a:b:c")),
        "{v}"
    );

    unsafe { bc_watch_free(watch) };
    unsafe { bc_client_free(client) };
}

/// `env.*` is constant for the life of a process, so there is nothing to
/// watch. The refusal is the library's own words, made without reaching the
/// daemon, and it is reported on the handle's first `bc_watch_next` like
/// every other deferred open failure.
#[test]
fn watch_open_on_an_env_key_is_refused() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let watch = open_watch(client, "env.HOME", None);

    let v = read(unsafe { bc_watch_next(watch, 2000) });
    assert_eq!(v["ok"], serde_json::json!(false), "{v}");
    // The library's own refusal, made without reaching the daemon — the
    // caller's input being wrong, not a server- or transport-level failure.
    assert_eq!(v["error"]["kind"], serde_json::json!("parse_error"), "{v}");
    assert_eq!(
        v["error"]["message"],
        serde_json::json!("env keys cannot be watched")
    );

    unsafe { bc_watch_free(watch) };
    unsafe { bc_client_free(client) };
}

/// `bc_watch_open` with a NULL `cwd` watches the global entry, as
/// `bc_get` reads it: the first event is the value put there.
#[test]
fn watch_open_with_null_cwd_watches_the_global_entry() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let key = cs("watch_null_cwd_test.field");
    let data = cs(r#"{"field":"global"}"#);
    let ns = cs("watch_null_cwd_test");
    let v = read(unsafe { bc_put(client, ns.as_ptr(), data.as_ptr(), ptr::null(), ptr::null()) });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    let watch = unsafe { bc_watch_open(client, key.as_ptr(), ptr::null(), ptr::null()) };
    assert!(!watch.is_null());

    let v = read(unsafe { bc_watch_next(watch, 2000) });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["outcome"], serde_json::json!("event"), "{v}");
    assert_eq!(v["data"], serde_json::json!("global"), "{v}");

    unsafe { bc_watch_free(watch) };
    unsafe { bc_client_free(client) };
}

/// Malformed `env_json` (not valid JSON) is the same deferred parse error as
/// `bc_get` gives it, surfaced on the first `bc_watch_next`.
#[test]
fn watch_open_with_malformed_env_json_is_a_parse_error() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let key = cs("watch_bad_env_test.field");
    let cwd = cs(CWD);
    let bad_env = cs("not json");
    let watch = unsafe { bc_watch_open(client, key.as_ptr(), cwd.as_ptr(), bad_env.as_ptr()) };
    assert!(!watch.is_null());

    let v = read(unsafe { bc_watch_next(watch, 2000) });
    assert_eq!(v["ok"], serde_json::json!(false), "{v}");
    assert_eq!(v["error"]["kind"], serde_json::json!("parse_error"), "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .is_some_and(|m| m.starts_with("malformed env_json:")),
        "{v}"
    );

    unsafe { bc_watch_free(watch) };
    unsafe { bc_client_free(client) };
}

/// A `cwd` that is not valid UTF-8 is the same deferred parse error, cheap
/// to build directly: `CString::new` only rejects an embedded NUL, so raw
/// non-UTF-8 bytes make a perfectly valid C string that fails only at the
/// `CStr::to_str` conversion inside `bc_watch_open`.
#[test]
fn watch_open_with_non_utf8_cwd_is_a_parse_error() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let key = cs("watch_non_utf8_cwd_test.field");
    let bad_cwd = CString::new(vec![0xFF, 0xFE]).unwrap();
    let watch = unsafe { bc_watch_open(client, key.as_ptr(), bad_cwd.as_ptr(), ptr::null()) };
    assert!(!watch.is_null());

    let v = read(unsafe { bc_watch_next(watch, 2000) });
    assert_eq!(v["ok"], serde_json::json!(false), "{v}");
    assert_eq!(v["error"]["kind"], serde_json::json!("parse_error"), "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .is_some_and(|m| m.starts_with("cwd is not valid UTF-8:")),
        "{v}"
    );

    unsafe { bc_watch_free(watch) };
    unsafe { bc_client_free(client) };
}

#[test]
fn watch_cancel_from_another_thread_unblocks_a_pending_next() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    put_static(client, "watch_cancel_test", r#"{"field":"v1"}"#);
    let watch = open_watch(client, "watch_cancel_test.field", None);

    // Consume the initial-value event so the next call genuinely blocks
    // waiting for a change that will never come.
    let first = read(unsafe { bc_watch_next(watch, 2000) });
    assert_eq!(first["outcome"], serde_json::json!("event"));

    let watch_ptr = SendPtr(watch);
    let watch_ptr = &watch_ptr;
    std::thread::scope(|scope| {
        let blocked = scope.spawn(|| {
            let v = read(unsafe { bc_watch_next(watch_ptr.0, -1) });
            (v, std::time::Instant::now())
        });

        // sleep-justified: the blocked call gives no sign that it has reached
        // its wait; this gives it time to, so the cancel ends a wait in
        // progress rather than one not yet begun.
        std::thread::sleep(std::time::Duration::from_millis(10));
        let cancelled_at = std::time::Instant::now();
        unsafe { bc_watch_cancel(watch_ptr.0) };

        let (v, returned_at) = blocked.join().unwrap();
        assert_eq!(v["ok"], serde_json::json!(true));
        assert_eq!(v["outcome"], serde_json::json!("cancelled"));
        let after = returned_at.saturating_duration_since(cancelled_at);
        assert!(
            after < std::time::Duration::from_millis(20),
            "a cancel ends a pending -1 (block indefinitely) call at once: {after:?}"
        );
    });

    unsafe { bc_watch_free(watch) };
    unsafe { bc_client_free(client) };
}

#[test]
fn watch_next_called_while_another_thread_is_in_it_yields_busy() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    put_static(client, "watch_busy_test", r#"{"field":"v1"}"#);
    let watch = open_watch(client, "watch_busy_test.field", None);

    // Consume the initial-value event so the holder below blocks.
    let first = read(unsafe { bc_watch_next(watch, 2000) });
    assert_eq!(first["outcome"], serde_json::json!("event"));

    let watch_ptr = SendPtr(watch);
    let watch_ptr = &watch_ptr;
    std::thread::scope(|scope| {
        // Refused while a call below holds the lock for an instant: it tries again.
        let holder = scope.spawn(|| {
            loop {
                let v = read(unsafe { bc_watch_next(watch_ptr.0, -1) });
                if v["error"]["kind"] != serde_json::json!("busy") {
                    return v;
                }
            }
        });

        // The holder gives no sign it has the lock but this: before it does,
        // a call here wins it and times out at once.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let v = read(unsafe { bc_watch_next(watch_ptr.0, 0) });
            if v["error"]["kind"] == serde_json::json!("busy") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "a second caller must be refused while the first holds the watch: {v}"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        unsafe { bc_watch_cancel(watch_ptr.0) };
        assert_eq!(holder.join().unwrap()["ok"], serde_json::json!(true));
    });

    unsafe { bc_watch_free(watch) };
    unsafe { bc_client_free(client) };
}
