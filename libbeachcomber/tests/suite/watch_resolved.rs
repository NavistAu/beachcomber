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

use crate::common::client::client_for;
use crate::common::daemon::DaemonGuard;

use libbeachcomber::resolve::{Inputs, ResolveError, Slot};
use libbeachcomber::watch::{ResolvedWatch, WatchError, WatchOutcome};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

/// The deadline a change is expected to arrive within.
const SOON: Duration = Duration::from_secs(2);

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
        now: std::time::SystemTime::now(),
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

/// Drive `w` until it emits `expected` (`None`: absent). An intermediate
/// emission is allowed — a template swapped for one whose refs are not there
/// yet is absent on its way — but a timeout, an EOF or an error is a failure.
fn wait_for(w: &mut ResolvedWatch<'_>, expected: impl Into<Option<Value>>) {
    let expected = expected.into();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match w.next(Some(SOON)) {
            Ok(WatchOutcome::Value(v)) if v == expected => return,
            Ok(WatchOutcome::Value(v)) => assert!(
                Instant::now() < deadline,
                "watch never emitted {expected:?}; last emission was {v:?}"
            ),
            other => panic!("expected {expected:?}, got {other:?}"),
        }
    }
}

/// `env.X` is constant for the process, so there is nothing to watch — and
/// the refusal costs no connection.
#[test]
fn env_keys_cannot_be_watched() {
    let client = client_for(Path::new("/nonexistent/beachcomber.sock"));
    let env = shell_env();
    for key in ["env", "env.HOME", "env['HOME']"] {
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
    writer
        .put("unrelated", json!({"k": 1}), None, None)
        .expect("put unrelated");
    // Neither is an emission, so the next one is the next change of value.
    writer
        .put(
            "scanline:template.widgets.foo",
            json!("{{ 'bye' }}"),
            None,
            None,
        )
        .expect("put a change");
    assert_eq!(
        value(w.next(Some(SOON))),
        Some(json!("bye")),
        "only a changed value is emitted, not every frame or an unread key's put"
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
            json!({"widgets": {"foo": "{{ comb.q.v }}"}}),
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
            json!("{{ comb.r.v }}"),
            None,
            None,
        )
        .expect("repoint the template");
    wait_for(&mut w, None);
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

/// A template that reads `comb[name]` with a computed name depends on the
/// provider the name names: that is what the render fetched, so it is what
/// the watch subscribes, and when the name moves to another provider the
/// subscription moves with it.
#[test]
fn a_computed_comb_name_is_a_dependency_and_moves_with_the_name() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);
    let writer = client_for(&guard.path);
    let env = shell_env();

    writer
        .put(
            "scanline:template",
            json!({"pick": "{{ comb[comb.sel.which].v }}"}),
            None,
            None,
        )
        .expect("put template");
    writer
        .put("sel", json!({"which": "qa"}), None, None)
        .expect("put sel");
    writer
        .put("qa", json!({"v": "a1"}), None, None)
        .expect("put qa");
    writer
        .put("qb", json!({"v": "b1"}), None, None)
        .expect("put qb");

    let mut w = ResolvedWatch::open(&client, "scanline.pick", &inputs(&env)).expect("open");
    assert_eq!(value(w.next(Some(SOON))), Some(json!("a1")));
    assert!(
        w.deps().contains(&("qa".to_string(), None)),
        "the provider the computed name named is a dependency: {:?}",
        w.deps()
    );

    writer
        .put("qa", json!({"v": "a2"}), None, None)
        .expect("put qa");
    wait_for(&mut w, json!("a2"));

    writer
        .put("sel", json!({"which": "qb"}), None, None)
        .expect("repoint sel");
    wait_for(&mut w, json!("b1"));
    assert!(
        w.deps().contains(&("qb".to_string(), None))
            && !w.deps().contains(&("qa".to_string(), None)),
        "the dependency moved with the name: {:?}",
        w.deps()
    );

    writer
        .put("qb", json!({"v": "b2"}), None, None)
        .expect("put qb");
    wait_for(&mut w, json!("b2"));
}

/// A watch on a key nothing answers yet follows the template a later `put`
/// gives it: the daemon's one subscription on the key's namespace is enough.
#[test]
fn a_watch_on_an_unknown_key_follows_a_later_template() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);
    let writer = client_for(&guard.path);
    let env = shell_env();

    let mut w = ResolvedWatch::open(&client, "scanline.widgets.foo", &inputs(&env)).expect("open");
    assert_eq!(value(w.next(Some(SOON))), None);

    writer
        .put("scanline:template", scanline_tree(), None, None)
        .expect("put template");
    wait_for(&mut w, json!("hi"));
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

/// `Some(Duration::ZERO)` still sees a frame that is already queued: it
/// makes one read pass before answering rather than checking the clock
/// first. No fixed sleep — a `put`'s frame lands on the daemon's own
/// schedule, so this polls with a zero timeout in a bounded loop until it is
/// seen; without the fix that poll can never see it and the loop times out.
#[test]
fn zero_timeout_polls_a_frame_already_queued() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);
    let writer = client_for(&guard.path);
    let env = shell_env();

    writer
        .put("my", json!({"a": 1}), None, None)
        .expect("put namespace");

    let mut w = ResolvedWatch::open(&client, "my.a", &inputs(&env)).expect("open");
    assert_eq!(value(w.next(Some(SOON))), Some(json!(1)));

    writer.put("my.a", json!(2), None, None).expect("put my.a");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match w.next(Some(Duration::ZERO)) {
            Ok(WatchOutcome::Value(Some(v))) if v == json!(2) => return,
            Ok(WatchOutcome::Timeout) => assert!(
                Instant::now() < deadline,
                "a zero-timeout poll never saw the queued frame"
            ),
            other => panic!("expected Timeout or Value(2), got {other:?}"),
        }
    }
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
    let setter = std::thread::spawn(move || cancel.cancel());
    match w.next(Some(Duration::from_secs(5))) {
        Ok(WatchOutcome::Cancelled) => {}
        other => panic!("a set cancel handle ends the wait; got {other:?}"),
    }
    setter.join().expect("join");
}

/// The daemon's health: its request count so far, this request included, and
/// the watches it holds.
fn health(client: &libbeachcomber::Client) -> libbeachcomber::DaemonHealth {
    match client
        .introspect(libbeachcomber::IntrospectSubject::Daemon, None)
        .expect("introspect")
    {
        libbeachcomber::IntrospectResponse::Daemon(health) => health,
        other => panic!("expected the daemon's health, got {other:?}"),
    }
}

/// A subscription's opening frame shows the node the opening resolve read —
/// a template node and data nodes alike — so it is no change and costs no
/// re-resolve: once the frames are in, the only request the daemon sees is
/// the one that counts them.
#[test]
fn opening_frames_that_match_the_resolve_cost_no_request() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);
    let writer = client_for(&guard.path);
    let env = shell_env();

    for dep in ["o1", "o2"] {
        writer
            .put(dep, json!({"x": "a"}), None, None)
            .expect("put a dep");
    }
    writer
        .put(
            "open:template",
            json!({"v": "{{ comb.o1.x ~ comb.o2.x }}"}),
            None,
            None,
        )
        .expect("put template");

    let mut w = ResolvedWatch::open(&client, "open.v", &inputs(&env)).expect("open");
    assert_eq!(w.deps().len(), 3, "{:?}", w.deps());
    assert_eq!(value(w.next(Some(SOON))), Some(json!("aa")));

    // Opening a subscription waits for nothing, so the daemon may take the
    // watch requests after `open` returns: count from when it holds all three.
    let deadline = Instant::now() + SOON;
    let before = loop {
        let now = health(&writer);
        if now.active_watchers == 3 {
            break now.requests_total;
        }
        assert!(
            Instant::now() < deadline,
            "the daemon never held the watches"
        );
    };
    match w.next(Some(Duration::from_millis(50))) {
        Ok(WatchOutcome::Timeout) => {}
        other => panic!("nothing changed, so nothing is emitted; got {other:?}"),
    }
    assert_eq!(
        health(&writer).requests_total - before,
        1,
        "only the count's own request"
    );
}

/// A change on the last of five deps is emitted as soon as it lands: the wait
/// is one `poll` over every stream, not a turn per stream.
#[test]
fn a_change_on_the_last_of_five_deps_is_emitted_at_once() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);
    let writer = client_for(&guard.path);
    let env = shell_env();

    for dep in ["d1", "d2", "d3", "d4"] {
        writer
            .put(dep, json!({"x": "a"}), None, None)
            .expect("put a dep");
    }
    writer
        .put(
            "fan:template",
            json!({"v": "{{ comb.d1.x ~ comb.d2.x ~ comb.d3.x ~ comb.d4.x }}"}),
            None,
            None,
        )
        .expect("put template");

    let mut w = ResolvedWatch::open(&client, "fan.v", &inputs(&env)).expect("open");
    assert_eq!(w.deps().len(), 5, "{:?}", w.deps());
    assert_eq!(w.deps()[4], ("d4.x".to_string(), None));
    assert_eq!(value(w.next(Some(SOON))), Some(json!("aaaa")));

    let start = Instant::now();
    writer
        .put("d4.x", json!("b"), None, None)
        .expect("put the last dep");
    assert_eq!(value(w.next(Some(SOON))), Some(json!("aaab")));
    let took = start.elapsed();
    assert!(took < Duration::from_millis(50), "{took:?}");
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
    match w.next(Some(SOON)) {
        Ok(WatchOutcome::Eof) => {}
        other => panic!("the daemon closing is an EOF, not a timeout; got {other:?}"),
    }
}

/// A template that cannot be resolved is someone's to go and fix: the watch
/// ends with the config error, naming the field. Here a leaf that reads
/// itself, since one that does not compile cannot be put.
#[test]
fn a_template_that_cannot_resolve_ends_the_watch() {
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
            json!("{{ scanline.widgets.foo }}"),
            None,
            None,
        )
        .expect("put a leaf that reads itself");
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
