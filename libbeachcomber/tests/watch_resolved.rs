//! `libbeachcomber::watch`: a watch that emits what `comb get` would print.
//!
//! Design §5.5's three key classes, against an in-process daemon: `env.X`
//! cannot be watched at all; a facet key is one plain daemon watch whose
//! frames are emitted as they arrive; a provider key resolves through
//! `libbeachcomber::resolve`, subscribes every key that resolution read plus
//! the provider's template tree, re-resolves on any frame and emits only when
//! the resolved value changed.
//!
//! Nothing here sleeps: changes are driven with `put` on a second client and
//! every wait is a deadline on `next`.

mod common;
use common::daemon::DaemonGuard;

use libbeachcomber::resolve::{Inputs, ResolveError, Slot};
use libbeachcomber::watch::{ResolvedWatch, WatchError, WatchOutcome};
use libbeachcomber::{Client, ClientConfig};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// The deadline a change is expected to arrive within.
const SOON: Duration = Duration::from_secs(2);

fn client_for(sock: &Path) -> Client {
    Client::with_config(ClientConfig {
        timeout: Duration::from_secs(2),
        auto_start: false,
        exec: false,
    })
    .with_socket_path(sock.to_path_buf())
}

/// The shell the design's §3.1 examples assume.
fn shell_env() -> HashMap<String, String> {
    HashMap::from([("SCANLINE_FOO".to_string(), "hi".to_string())])
}

/// Every watch here addresses the global entry, which is also where the
/// `put`s that drive them land.
fn inputs(env: &HashMap<String, String>) -> Inputs<'_> {
    Inputs {
        cwd: "/",
        env,
        slot: Slot::Global,
        force: false,
        wait: false,
        allow_exec: false,
    }
}

/// The §3.1 template tree, as far as these tests need it.
fn scanline_tree() -> Value {
    json!({
        "theme": "{{ env.SCANLINE_THEME or 'dark' }}",
        "widgets": {"foo": "{{ env.SCANLINE_FOO }}"}
    })
}

fn value(outcome: Result<WatchOutcome, WatchError>) -> Option<Value> {
    match outcome {
        Ok(WatchOutcome::Value(v)) => v,
        other => panic!("expected a value, got {other:?}"),
    }
}

/// Drive `w` until it emits `expected`. An intermediate emission is allowed —
/// a template swapped for one whose refs are not there yet resolves to `""`
/// on its way — but a timeout, an EOF or an error is a failure.
fn wait_for(w: &mut ResolvedWatch<'_>, expected: Value) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match w.next(Some(SOON)) {
            Ok(WatchOutcome::Value(Some(v))) if v == expected => return,
            Ok(WatchOutcome::Value(v)) => assert!(
                Instant::now() < deadline,
                "watch never emitted {expected}; last emission was {v:?}"
            ),
            other => panic!("expected {expected}, got {other:?}"),
        }
    }
}

/// `env.X` is constant for the process, so there is nothing to watch — and
/// the refusal costs no connection.
#[test]
fn env_keys_cannot_be_watched() {
    let client = client_for(Path::new("/nonexistent/beachcomber.sock"));
    let env = shell_env();
    for key in ["env", "env.HOME"] {
        match ResolvedWatch::open(&client, key, &inputs(&env)) {
            Err(WatchError::EnvKey(message)) => {
                assert_eq!(message, "env keys cannot be watched", "{key}")
            }
            Err(other) => panic!("{key}: expected EnvKey, got {other:?}"),
            Ok(_) => panic!("{key}: expected EnvKey, got a watch"),
        }
    }
}

/// A facet key has no template half: one plain daemon watch, the frames
/// emitted as they arrive.
#[test]
fn a_facet_key_is_a_raw_watch() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);
    let writer = client_for(&guard.path);
    let env = shell_env();

    writer
        .put("p:template", json!({"x": "{{ 1 }}"}), None, None)
        .expect("put template");

    let mut w = ResolvedWatch::open(&client, "p:template", &inputs(&env)).expect("open");
    assert_eq!(
        value(w.next(Some(SOON))),
        Some(json!({"x": "{{ 1 }}"})),
        "the first frame is the raw tree, not a rendered value"
    );

    writer
        .put("p:template.x", json!("{{ 2 }}"), None, None)
        .expect("put leaf");
    wait_for(&mut w, json!({"x": "{{ 2 }}"}));
}

/// A provider key emits the rendered value, and a template `put` after the
/// watch started is picked up — that is the `<p>:template` subscription.
/// An unrelated `put` is not a frame.
#[test]
fn a_provider_key_emits_the_resolved_value_and_follows_its_template() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);
    let writer = client_for(&guard.path);
    let env = shell_env();

    writer
        .put("scanline:template", scanline_tree(), None, None)
        .expect("put template");

    let mut w = ResolvedWatch::open(&client, "scanline.widgets.foo", &inputs(&env)).expect("open");
    assert_eq!(
        w.deps(),
        [("scanline.widgets.foo".to_string(), None)],
        "the resolution read the key itself and nothing else"
    );
    assert_eq!(value(w.next(Some(SOON))), Some(json!("hi")));

    writer
        .put(
            "scanline:template.widgets.foo",
            json!("{{ env.SCANLINE_FOO ~ \"!\" }}"),
            None,
            None,
        )
        .expect("put leaf");
    wait_for(&mut w, json!("hi!"));

    // A frame is not an emission: this template is a different one, so the
    // daemon sends it, and it renders to the value already emitted.
    writer
        .put(
            "scanline:template.widgets.foo",
            json!("{{ \"hi\" ~ \"!\" }}"),
            None,
            None,
        )
        .expect("put another spelling");
    assert!(
        matches!(
            w.next(Some(Duration::from_millis(500))),
            Ok(WatchOutcome::Timeout)
        ),
        "only a changed value is emitted, not every frame"
    );

    writer
        .put("unrelated", json!({"k": 1}), None, None)
        .expect("put unrelated");
    assert!(
        matches!(
            w.next(Some(Duration::from_millis(500))),
            Ok(WatchOutcome::Timeout)
        ),
        "a key this resolution never read must not wake the watch"
    );
}

/// The subscription set is the latest resolution's: a ref the template names
/// is watched, and a ref it stops naming for a new one moves with it.
#[test]
fn subscriptions_follow_the_deps() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);
    let writer = client_for(&guard.path);
    let env = shell_env();

    writer
        .put(
            "scanline:template",
            json!({"widgets": {"foo": "{{ cache.q.v }}"}}),
            None,
            None,
        )
        .expect("put template");
    writer.put("q.v", json!("1"), None, None).expect("put q.v");

    let mut w = ResolvedWatch::open(&client, "scanline.widgets.foo", &inputs(&env)).expect("open");
    assert_eq!(
        w.deps(),
        [
            ("scanline.widgets.foo".to_string(), None),
            ("q.v".to_string(), None)
        ],
        "the resolution read the key and the ref its template names"
    );
    assert_eq!(value(w.next(Some(SOON))), Some(json!("1")));

    writer.put("q.v", json!("2"), None, None).expect("put q.v");
    wait_for(&mut w, json!("2"));

    writer
        .put(
            "scanline:template.widgets.foo",
            json!("{{ cache.r.v }}"),
            None,
            None,
        )
        .expect("repoint the template");
    wait_for(&mut w, json!(""));
    assert_eq!(
        w.deps(),
        [
            ("scanline.widgets.foo".to_string(), None),
            ("r.v".to_string(), None)
        ],
        "the ref the template stopped naming is gone from the deps"
    );

    writer.put("r.v", json!("3"), None, None).expect("put r.v");
    wait_for(&mut w, json!("3"));
}

/// A miss is an answer: nothing is stored there, the watch stays open.
#[test]
fn a_miss_is_an_empty_value() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);
    let writer = client_for(&guard.path);
    let env = shell_env();

    writer
        .put("my", json!({"a": 1}), None, None)
        .expect("put namespace");

    let mut w = ResolvedWatch::open(&client, "my.b", &inputs(&env)).expect("open");
    assert_eq!(value(w.next(Some(SOON))), None);

    writer.put("my.b", json!(7), None, None).expect("put my.b");
    wait_for(&mut w, json!(7));
}

/// The cancel handle ends a `next` that is already blocking, from another
/// thread.
#[test]
fn the_cancel_handle_ends_a_blocking_next() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);
    let writer = client_for(&guard.path);
    let env = shell_env();

    writer
        .put("my", json!({"a": 1}), None, None)
        .expect("put namespace");

    let mut w = ResolvedWatch::open(&client, "my.a", &inputs(&env)).expect("open");
    assert_eq!(value(w.next(Some(SOON))), Some(json!(1)));

    let cancel = w.cancel_handle();
    let setter = std::thread::spawn(move || cancel.store(true, Ordering::SeqCst));
    assert!(
        matches!(
            w.next(Some(Duration::from_secs(5))),
            Ok(WatchOutcome::Cancelled)
        ),
        "a set cancel handle ends the wait"
    );
    setter.join().expect("join");
}

/// The daemon going away ends the stream.
#[test]
fn the_daemon_closing_ends_the_watch() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);
    let writer = client_for(&guard.path);
    let env = shell_env();

    writer
        .put("my", json!({"a": 1}), None, None)
        .expect("put namespace");

    let mut w = ResolvedWatch::open(&client, "my.a", &inputs(&env)).expect("open");
    assert_eq!(value(w.next(Some(SOON))), Some(json!(1)));

    drop(guard);
    assert!(
        matches!(w.next(Some(SOON)), Ok(WatchOutcome::Eof)),
        "the daemon closing is an EOF, not a timeout"
    );
}

/// A template that does not compile is someone's to go and fix: the watch
/// ends with the config error, naming the field.
#[test]
fn a_template_that_does_not_compile_ends_the_watch() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);
    let writer = client_for(&guard.path);
    let env = shell_env();

    writer
        .put("scanline:template", scanline_tree(), None, None)
        .expect("put template");

    let mut w = ResolvedWatch::open(&client, "scanline.widgets.foo", &inputs(&env)).expect("open");
    assert_eq!(value(w.next(Some(SOON))), Some(json!("hi")));

    writer
        .put(
            "scanline:template.widgets.foo",
            json!("{{ 1 + }}"),
            None,
            None,
        )
        .expect("put a broken leaf");
    match w.next(Some(SOON)) {
        Err(WatchError::Resolve(ResolveError::Config(message))) => {
            assert!(
                message.contains("scanline.widgets.foo"),
                "the error names the field: {message}"
            )
        }
        other => panic!("expected a config error, got {other:?}"),
    }
}
