//! A memo's lifecycle, as the daemon keeps it: the `memo` op makes the memo's
//! address a lifecycle entry of its own, its `watch` paths register while it
//! is Active, a change to one drops its hash, its ttl drops its hash and
//! starts its decay (and its watches go), and the end of its decay evicts it.
//! Reads never renew it. These drive an in-process daemon on the shipped
//! defaults (a polling watcher) over its socket, as the resolver's write-back
//! would.

use crate::common::poll::poll_until;
use crate::common::socket::{ask, watches};
use crate::shipped::scenario;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

const H: &str = "9c6001d328a94690";

/// Store a template at `demo.f` and its memo for a reader at `at`.
fn memo(sock: &Path, at: &Path, watch: &Path, ttl: Option<&str>) {
    let put = json!({"op": "put", "key": "demo:template.f", "data": "{{ 'x' }}"});
    assert_eq!(ask(sock, put)["ok"], json!(true));
    let write = json!({"op": "memo", "key": "demo.f", "path": at, "hash": H, "data": "prod",
                       "watch": [watch], "ttl": ttl});
    let answer = ask(sock, write);
    assert_eq!(answer["ok"], json!(true), "{answer}");
}

/// `demo:cache.f` at `at`: the memo's record while it has one.
fn record(sock: &Path, at: &Path) -> Value {
    ask(
        sock,
        json!({"op": "get", "key": "demo:cache.f", "path": at}),
    )["data"]
        .clone()
}

/// The entries `introspect <subject>` lists under its own name.
fn entries(sock: &Path, subject: &str) -> Vec<Value> {
    ask(sock, json!({"op": "introspect", "subject": subject}))["data"][subject]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn wait<T: std::fmt::Debug>(
    what: &str,
    expected: &str,
    poll: impl FnMut() -> T,
    holds: impl Fn(&T) -> bool,
) -> T {
    poll_until(
        what,
        expected,
        Duration::from_secs(10),
        Duration::from_millis(5),
        poll,
        holds,
    )
}

/// A watch path that does not exist yet is watched through its directory,
/// and its creation drops the memo's hash; the watch then moves onto the
/// file. `introspect demand` names the memo's entry by its node.
#[test]
fn an_absent_watch_path_is_watched_through_its_parent_and_its_creation_drops_the_hash() {
    let s = scenario();
    let sock = &s.daemon.socket.path;
    let file = s.project.join("config");
    memo(sock, &s.project, &file, None);

    let path = file.to_string_lossy().into_owned();
    wait(
        "introspect watches",
        "the memo's watch, Active through the directory that exists",
        || watches(sock),
        |w| *w == [(path.clone(), json!({"state": "active", "via": s.project}))],
    );
    let named = json!({"provider": "demo", "path": s.project, "source": "memo", "node": "f"});
    assert!(
        entries(sock, "demand").iter().any(|e| {
            let mut e = e.clone();
            e.as_object_mut().unwrap().remove("last_query_secs_ago");
            e == named
        }),
        "{:?}",
        entries(sock, "demand")
    );
    assert_eq!(record(sock, &s.project)["hash"], json!(H));

    std::fs::write(&file, "ctx").unwrap();
    wait(
        "demo:cache.f",
        "the memo's record without its hash",
        || record(sock, &s.project),
        |r| r["source"] == json!("memo") && r.get("hash").is_none(),
    );
    wait(
        "introspect watches",
        "the watch on the file itself",
        || watches(sock),
        |w| *w == [(path.clone(), json!({"state": "active"}))],
    );
}

/// A memo with a one-second ttl: its status row is a lifecycle row of depth
/// one; reads do not keep it; once its ttl has passed its hash is gone, it
/// decays and lets go of its watch (`introspect watches` is empty and
/// `introspect lifecycle` names it), and one decay step later it is evicted,
/// its value and its record with it.
///
/// The ttl (1 s) and its one decay step (2 s) pass on the daemon's clock,
/// which the test moves on: each ends at the scheduler's next tick.
#[test]
fn a_memo_decays_lets_go_of_its_watch_and_is_evicted() {
    let s = scenario();
    let sock = &s.daemon.socket.path;
    let file = s.project.join("config");
    std::fs::write(&file, "ctx").unwrap();
    memo(sock, &s.project, &file, Some("1s"));
    let path = file.to_string_lossy().into_owned();
    wait(
        "introspect watches",
        "the memo's watch, Active",
        || watches(sock),
        |w| *w == [(path.clone(), json!({"state": "active"}))],
    );

    let rows = ask(sock, json!({"op": "status"}))["data"].clone();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["provider"] == json!("demo:memo") && r["field"] == json!("f"))
        .cloned()
        .unwrap_or_else(|| panic!("a demo:memo row: {rows}"));
    assert_eq!(
        row["kind"],
        json!({"kind": "lifecycle", "decay": 0, "depth": 1, "watches_files": true}),
        "{row}"
    );
    for _ in 0..3 {
        let get = json!({"op": "get", "key": "demo.f", "path": s.project});
        assert_eq!(ask(sock, get)["memo"]["data"], json!("prod"));
    }

    s.daemon.advance(Duration::from_secs(1));
    wait(
        "introspect watches",
        "no watch once the memo has decayed",
        || watches(sock),
        Vec::is_empty,
    );
    assert!(
        record(sock, &s.project).get("hash").is_none(),
        "the ttl dropped it"
    );
    let decaying: Vec<Value> = entries(sock, "lifecycle")
        .into_iter()
        .map(|mut e| {
            e.as_object_mut().unwrap().remove("elapsed_secs");
            e
        })
        .collect();
    assert_eq!(
        decaying,
        [
            json!({"provider": "demo", "path": s.project, "source": "memo", "node": "f",
                "stage": "Decay1"})
        ]
    );

    s.daemon.advance(Duration::from_secs(2));
    wait(
        "demo:memo.f",
        "the memo evicted",
        || {
            ask(
                sock,
                json!({"op": "get", "key": "demo:memo.f", "path": s.project}),
            )["data"]
                .clone()
        },
        Value::is_null,
    );
    assert_eq!(
        record(sock, &s.project),
        json!({"stale": false, "source": "template"}),
        "the template leaf with no memo"
    );
    assert!(entries(sock, "lifecycle").is_empty());
}
