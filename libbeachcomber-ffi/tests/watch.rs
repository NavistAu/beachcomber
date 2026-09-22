//! Watch: `bc_watch_open`, `bc_watch_next`, `bc_watch_cancel`,
//! `bc_watch_free`.
//!
//! Since Plan 4 Task 3 a watch is a *resolved* watch: `bc_watch_open` takes
//! the same `cwd`/`env_json` pair `bc_resolve` does, every event is the
//! value `bc_resolve` would return for that key, and the fan-in underneath
//! subscribes to every key the resolution read plus the provider's
//! `<p>:template`, so editing a template the value is rendered from fires
//! the watch.
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
    BcClient, BcWatch, bc_client_free, bc_client_new, bc_put, bc_resolve, bc_watch_cancel,
    bc_watch_free, bc_watch_next, bc_watch_open,
};

/// The directory every watch here addresses. Nothing is stored at it: each
/// fixture `put`s globally, and the global entry is the last link of every
/// path's chain (canon `field_resolution.md` §"Path resolution").
const CWD: &str = "/tmp";

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

#[test]
fn watch_next_first_call_yields_an_event_outcome() {
    // A provider key is resolved once at open, so the very first
    // `bc_watch_next` call answers with the current value without waiting for
    // a change — and that value is the bare resolved value, not the daemon's
    // `{data, age_ms, stale}` frame.
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    put_static(client, "watch_first_test", r#"{"field":"v1"}"#);
    let watch = open_watch(client, "watch_first_test.field", None);

    let v = read(unsafe { bc_watch_next(watch, 2000) });
    assert_eq!(v["ok"], serde_json::json!(true));
    assert_eq!(v["outcome"], serde_json::json!("event"));
    assert_eq!(v["data"], serde_json::json!("v1"));

    unsafe { bc_watch_free(watch) };
    unsafe { bc_client_free(client) };
}

/// The entire point of `open_owned`/`Arc<Client>`: a `BcWatch` holds its own
/// share of the client, so freeing the client handle does not invalidate a
/// watch still in use — free order between the two is independent. Nothing
/// currently fails if this regresses to a borrow.
#[test]
fn watch_next_works_after_the_client_that_opened_it_is_freed() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    put_static(client, "watch_free_order_test", r#"{"field":"v1"}"#);
    let watch = open_watch(client, "watch_free_order_test.field", None);

    unsafe { bc_client_free(client) };

    let v = read(unsafe { bc_watch_next(watch, 2000) });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["outcome"], serde_json::json!("event"), "{v}");
    assert_eq!(v["data"], serde_json::json!("v1"));

    unsafe { bc_watch_free(watch) };
}

/// The equivalence Plan 3 models the whole surface on: a watch's first event
/// is exactly what `bc_resolve` returns for the same key, `cwd` and
/// `env_json` — template rendered, environment applied.
#[test]
fn watch_first_event_equals_bc_resolve_of_the_same_key() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let env = r#"{"EQ_VALUE":"rendered"}"#;
    put_static(client, "eqprov:template", r#"{"f":"{{ env.EQ_VALUE }}"}"#);

    let resolved = read(unsafe {
        bc_resolve(
            client,
            cs("eqprov.f").as_ptr(),
            cs(CWD).as_ptr(),
            cs(env).as_ptr(),
        )
    });
    assert_eq!(resolved["ok"], serde_json::json!(true), "{resolved}");
    assert_eq!(resolved["data"], serde_json::json!("rendered"));

    let watch = open_watch(client, "eqprov.f", Some(env));
    let v = read(unsafe { bc_watch_next(watch, 2000) });
    assert_eq!(v["outcome"], serde_json::json!("event"), "{v}");
    assert_eq!(
        v["data"], resolved["data"],
        "the first event must be bc_resolve's answer"
    );

    unsafe { bc_watch_free(watch) };
    unsafe { bc_client_free(client) };
}

/// Editing the template the watched value is rendered from fires the watch:
/// `tplprov:template` is not the address being watched, but the resolution
/// read it, so the fan-in is subscribed to it.
#[test]
fn a_put_to_the_template_fires_the_watch() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    put_static(client, "tplprov:template", r#"{"f":"{{ 'v1' }}"}"#);
    let watch = open_watch(client, "tplprov.f", None);

    let first = read(unsafe { bc_watch_next(watch, 2000) });
    assert_eq!(first["data"], serde_json::json!("v1"), "{first}");

    put_static(client, "tplprov:template", r#"{"f":"{{ 'v2' }}"}"#);

    let v = read(unsafe { bc_watch_next(watch, 5000) });
    assert_eq!(v["outcome"], serde_json::json!("event"), "{v}");
    assert_eq!(v["data"], serde_json::json!("v2"));

    unsafe { bc_watch_free(watch) };
    unsafe { bc_client_free(client) };
}

/// A watch only emits when *its* value changes. A `put` to an unrelated
/// namespace touches no key this resolution read, so the wait runs out.
#[test]
fn a_put_to_an_unrelated_key_does_not_fire_the_watch() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    put_static(client, "watch_unrelated_test", r#"{"field":"v1"}"#);
    let watch = open_watch(client, "watch_unrelated_test.field", None);

    let first = read(unsafe { bc_watch_next(watch, 2000) });
    assert_eq!(first["data"], serde_json::json!("v1"), "{first}");

    put_static(client, "watch_elsewhere_test", r#"{"other":"v9"}"#);

    let v = read(unsafe { bc_watch_next(watch, 300) });
    assert_eq!(v["ok"], serde_json::json!(true), "{v}");
    assert_eq!(v["outcome"], serde_json::json!("timeout"), "{v}");

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

/// `bc_watch_open` with a NULL `cwd` is a deferred error, not a NULL return:
/// `cwd` is required the way it is for `bc_resolve`, and the refusal names
/// the argument.
#[test]
fn watch_open_with_null_cwd_is_a_parse_error_naming_cwd() {
    let daemon = DaemonGuard::spawn();
    let client = client_for(&daemon.path);
    let key = cs("watch_null_cwd_test.field");
    let watch = unsafe { bc_watch_open(client, key.as_ptr(), ptr::null(), ptr::null()) };
    assert!(!watch.is_null());

    let v = read(unsafe { bc_watch_next(watch, 2000) });
    assert_eq!(v["ok"], serde_json::json!(false), "{v}");
    assert_eq!(v["error"]["kind"], serde_json::json!("parse_error"), "{v}");
    assert_eq!(
        v["error"]["message"],
        serde_json::json!("cwd must not be NULL"),
        "{v}"
    );

    unsafe { bc_watch_free(watch) };
    unsafe { bc_client_free(client) };
}

/// Malformed `env_json` (not valid JSON) is the same deferred parse error as
/// `bc_resolve` gives it, surfaced on the first `bc_watch_next`.
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
    put_static(client, "watch_busy_test", r#"{"field":"v1"}"#);
    let watch = open_watch(client, "watch_busy_test.field", None);

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
