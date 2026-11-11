//! The wire's shape is held to `PROTOCOL_VERSION`: a change to a request or
//! a response without a version bump fails here.
//!
//! Every op's request and every wire struct is built with each field set
//! (an option `Some`, a list non-empty, a flag `true`), so serialised it
//! shows every field it can carry; its shape is that JSON with each value
//! replaced by its type. The struct literals name every field and the
//! `match`es every variant, so a field or op added to `protocol` does not
//! compile here until it is given a value. The shape is checked against
//! `tests/wire_shape.json`, taken at the version it names.

use libbeachcomber::protocol::*;
use serde_json::{Map, Value, json};
use std::time::Duration;

const SNAPSHOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/wire_shape.json");
const REFRESH: &str =
    "UPDATE_WIRE_SHAPE=1 cargo nextest run -p libbeachcomber -E 'test(wire_shape)'";

fn line() -> LogLine {
    LogLine {
        level: Level::Warn,
        msg: "m".into(),
        origin: "source(s)".into(),
        count: Some(2),
        path: Some("/p".into()),
        key: Some("p.f".into()),
    }
}

/// Every op, each field set. A new op is a compile error in [`op`] until it
/// has a sample here.
fn requests() -> Vec<Request> {
    let requests = vec![
        Request::Get {
            key: "p.f".into(),
            path: Some("/p".into()),
            force: true,
            wait: Wait::For(Duration::from_millis(50)),
            log: Some(Level::Debug),
        },
        Request::Refresh {
            key: "p".into(),
            path: Some("/p".into()),
        },
        Request::Status {},
        Request::Put {
            key: "p".into(),
            data: json!({"f": 1}),
            ttl: Some("1m".into()),
            path: Some("/p".into()),
            persist: true,
        },
        Request::Watch {
            key: "p".into(),
            path: Some("/p".into()),
        },
        Request::Memo {
            key: "p.f".into(),
            path: Some("/p".into()),
            hash: "0000000000000000".into(),
            data: Some(json!("v")),
            watch: vec!["/p/f".into()],
            ttl: Some("10s".into()),
            log: vec![line()],
        },
        Request::Introspect {
            subject: IntrospectSubject::Daemon,
            duration_secs: Some(1),
        },
        Request::Hello {},
        Request::Layers {
            key: "p".into(),
            path: Some("/p".into()),
        },
    ];
    let mut ops: Vec<&str> = requests.iter().map(op).collect();
    ops.dedup();
    assert_eq!(ops.len(), requests.len(), "one sample per op");
    requests
}

fn op(request: &Request) -> &'static str {
    match request {
        Request::Get { .. } => "get",
        Request::Refresh { .. } => "refresh",
        Request::Status {} => "status",
        Request::Put { .. } => "put",
        Request::Watch { .. } => "watch",
        Request::Memo { .. } => "memo",
        Request::Introspect { .. } => "introspect",
        Request::Hello {} => "hello",
        Request::Layers { .. } => "layers",
    }
}

fn row_kinds() -> Vec<RowKind> {
    let kinds = vec![
        RowKind::Lifecycle {
            decay: 1,
            depth: 4,
            watches_files: true,
        },
        RowKind::Once,
        RowKind::Static,
        RowKind::Transient,
        RowKind::Rendered,
    ];
    for kind in &kinds {
        match kind {
            RowKind::Lifecycle { .. }
            | RowKind::Once
            | RowKind::Static
            | RowKind::Transient
            | RowKind::Rendered => {}
        }
    }
    kinds
}

fn levels() -> Vec<Level> {
    let all = vec![
        Level::Debug,
        Level::Info,
        Level::Warn,
        Level::Error,
        Level::Fatal,
    ];
    for level in &all {
        match level {
            Level::Debug | Level::Info | Level::Warn | Level::Error | Level::Fatal => {}
        }
    }
    all
}

/// The wire's shape now: each request by op, the response envelope and the
/// structs `data` carries, and the values each enum is written as.
fn shape() -> Value {
    let mut ops = Map::new();
    for request in requests() {
        ops.insert(op(&request).into(), shape_of(&to(&request)));
    }
    let response = Response {
        ok: true,
        data: Some(json!("d")),
        age_ms: Some(1),
        error: Some("e".into()),
        template: Some(json!("t")),
        rest: Some("r".into()),
        memo: Some(json!({})),
        var: Some(json!({})),
        var_memo: Some(json!({})),
        log: vec![line()],
    };
    let row = CacheRow {
        provider: "p".into(),
        path: Some("/p".into()),
        source: "source(s)".into(),
        field: "f".into(),
        value: json!("v"),
        age_ms: Some(1),
        kind: Some(row_kinds().remove(0)),
        poll_interval_secs: Some(1),
        poll_count: Some(1),
        polls_elapsed: Some(1),
        next_poll_in_secs: Some(1),
        failure: Some(FailureSnapshot {
            consecutive_failures: 1,
            suppressed_until_unix_ms: Some(1),
            stopped: true,
            last_error: Some("e".into()),
        }),
        shadowed: true,
        warn: Some("w".into()),
    };
    let hello = HelloInfo {
        protocol_version: "x".into(),
        daemon_version: "y".into(),
    };
    let health = DaemonHealth {
        pid: 1,
        version: "v".into(),
        uptime_secs: 1,
        socket_path: "/s".into(),
        config_path: Some("/c".into()),
        requests_total: 1,
        in_flight: 1,
        active_watchers: 1,
        cache_entries: 1,
        watch_backend: "native".into(),
        reaper: Some(ReaperStatus {
            armed: true,
            visibility: "confined".into(),
            sweeps: 1,
            reaped: 1,
            kill_denied: 1,
        }),
        verdicts: vec![Verdict {
            level: "PASS".into(),
            message: "m".into(),
        }],
    };
    let names = |values: Vec<Value>| -> Value { values.into() };
    json!({
        "request": ops,
        "response": shape_of(&to(&response)),
        "status_row": shape_of(&to(&row)),
        "hello": shape_of(&to(&hello)),
        "introspect_daemon": shape_of(&to(&health)),
        "enums": {
            "level": names(levels().iter().map(to).collect()),
            "introspect_subject": names(IntrospectSubject::ALL.iter().map(to).collect()),
            "row_kind": names(row_kinds().iter().map(|k| shape_of(&to(k))).collect()),
            "wait": names([Wait::Budget, Wait::For(Duration::from_millis(1500)), Wait::Forever]
                .iter().map(to).collect()),
        },
    })
}

/// `v` with each scalar replaced by its type's name; a list by the shape of
/// its items, once each.
fn shape_of(v: &Value) -> Value {
    match v {
        Value::Null => "null".into(),
        Value::Bool(_) => "bool".into(),
        Value::Number(_) => "number".into(),
        Value::String(_) => "string".into(),
        Value::Array(items) => {
            let mut shapes: Vec<Value> = Vec::new();
            for item in items.iter().map(shape_of) {
                if !shapes.contains(&item) {
                    shapes.push(item);
                }
            }
            shapes.into()
        }
        Value::Object(map) => {
            let mut out: Map<String, Value> =
                map.iter().map(|(k, v)| (k.clone(), shape_of(v))).collect();
            // A tag is a value worth keeping: which op, which kind.
            if let Some(Value::String(op)) = map.get("op") {
                out.insert("op".into(), op.clone().into());
            }
            if let Some(Value::String(kind)) = map.get("kind") {
                out.insert("kind".into(), kind.clone().into());
            }
            out.into()
        }
    }
}

fn to<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).expect("a wire type serialises")
}

#[test]
fn wire_shape_changes_only_with_the_protocol_version() {
    let now = json!({"protocol_version": PROTOCOL_VERSION, "shape": shape()});
    if std::env::var_os("UPDATE_WIRE_SHAPE").is_some() {
        let text = serde_json::to_string_pretty(&now).unwrap() + "\n";
        std::fs::write(SNAPSHOT, text).unwrap();
        return;
    }
    let taken: Value = serde_json::from_str(
        &std::fs::read_to_string(SNAPSHOT).expect("tests/wire_shape.json is checked in"),
    )
    .unwrap();
    let same_version = taken["protocol_version"] == now["protocol_version"];
    let same_shape = taken["shape"] == now["shape"];
    assert!(
        same_shape || !same_version,
        "the wire's shape changed but PROTOCOL_VERSION is still {PROTOCOL_VERSION}: bump it as \
         the Versioning table in website/docs/reference/protocol-reference.md says (minor for a \
         new op or optional field, major for anything removed, renamed or changed), then \
         refresh the snapshot: {REFRESH}"
    );
    assert!(
        same_shape && same_version,
        "PROTOCOL_VERSION is {PROTOCOL_VERSION} but tests/wire_shape.json was taken at {}: \
         refresh it: {REFRESH}",
        taken["protocol_version"]
    );
}
