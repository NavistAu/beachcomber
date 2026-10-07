// Phase 3: typed-shape integration tests for the public SDK crate.

use crate::common::daemon::DaemonGuard;

use libbeachcomber::{Client, ClientConfig};
use std::time::Duration;

fn client_for(sock: &std::path::Path) -> Client {
    Client::with_config(ClientConfig {
        timeout: Duration::from_secs(2),
        auto_start: false,
        exec: false,
    })
    .with_socket_path(sock.to_path_buf())
}

#[test]
fn status_returns_typed_cache_rows() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);

    client.put_null("phase3_marker", None).ok(); // ensure something virtual registered

    // The put_null call above may be ignored if the op fails; status should
    // still succeed. What we're really asserting is the typed shape.
    let rows = client.status().expect("status");

    // The shape deserialized — each row has a `provider` string and a
    // boolean `stale`. Content assertions live in the conformance suite
    // and the round-trip test below; here we just confirm the typed
    // decoding path didn't panic.
    for row in &rows {
        assert!(!row.provider.is_empty());
    }
}

use libbeachcomber::{CombResult, IntrospectResponse, IntrospectSubject};

#[test]
fn put_with_data_then_get_round_trips() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);

    client
        .put(
            "phase3_put",
            serde_json::json!({"color": "purple", "count": 3}),
            None,
            None,
        )
        .expect("put");

    match client.get("phase3_put.color", None).expect("get") {
        CombResult::Hit { data, .. } => {
            assert_eq!(data.as_text().as_deref(), Some("purple"));
        }
        other => panic!("put followed by get should hit, got {other:?}"),
    }
}

/// A raw `get` carries the template half the daemon holds at the address
/// (one-tree design §3.4, §5.3): a template-only node is `Template` — not a
/// hit, not a miss — a valued node is `Hit` with whatever template rides
/// along, and past a template string the leftover address is `rest`.
#[test]
fn get_carries_the_template_half() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);

    client
        .put(
            "p:template",
            serde_json::json!({"t": "{{ 1 }}"}),
            None,
            None,
        )
        .expect("put template");
    client
        .put("p", serde_json::json!({"v": 1}), None, None)
        .expect("put data");

    let template_only = client.get("p.t", None).expect("get p.t");
    assert_eq!(
        template_only,
        CombResult::Template {
            template: serde_json::json!("{{ 1 }}"),
            rest: None,
        }
    );
    assert!(!template_only.is_hit());
    assert!(!template_only.is_miss());
    assert_eq!(
        template_only.template(),
        Some(&serde_json::json!("{{ 1 }}"))
    );

    match client.get("p.v", None).expect("get p.v") {
        CombResult::Hit { data, template, .. } => {
            assert_eq!(data.as_value(), &serde_json::json!(1));
            assert_eq!(template, None);
        }
        other => panic!("a valued node is a hit, got {other:?}"),
    }

    client
        .put("p:template.v", serde_json::json!("{{ 2 }}"), None, None)
        .expect("put template over data");
    match client.get("p.v", None).expect("get p.v with both halves") {
        CombResult::Hit {
            data,
            template,
            rest,
            ..
        } => {
            assert_eq!(data.as_value(), &serde_json::json!(1));
            assert_eq!(template, Some(serde_json::json!("{{ 2 }}")));
            assert_eq!(rest, None);
        }
        other => panic!("both halves is still a hit, got {other:?}"),
    }

    assert_eq!(
        client.get("p.nope", None).expect("get p.nope"),
        CombResult::Miss
    );

    assert_eq!(
        client.get("p.t.x", None).expect("get p.t.x"),
        CombResult::Template {
            template: serde_json::json!("{{ 1 }}"),
            rest: Some("x".to_string()),
        }
    );
}

#[test]
fn introspect_daemon_returns_typed_health() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);
    let resp = client
        .introspect(IntrospectSubject::Daemon, None)
        .expect("introspect");
    match resp {
        IntrospectResponse::Daemon(health) => {
            assert!(health.pid > 0, "pid must be positive, got {}", health.pid);
            assert!(!health.version.is_empty());
        }
        IntrospectResponse::Other(_) => panic!("daemon subject must return Daemon variant"),
    }
}

#[test]
fn introspect_providers_returns_other_variant() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);
    let resp = client
        .introspect(IntrospectSubject::Providers, None)
        .expect("introspect providers");
    match resp {
        IntrospectResponse::Other(_) => {}
        IntrospectResponse::Daemon(_) => panic!("providers must return Other variant"),
    }
}

#[test]
fn watch_receives_initial_event() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);

    client
        .put("phase3_watch", serde_json::json!({"x": 1}), None, None)
        .expect("put");

    let mut stream = client.watch("phase3_watch.x", None).expect("watch");
    let event = stream
        .next_event()
        .expect("watch event")
        .expect("non-empty stream");
    assert!(
        event.data.is_some(),
        "initial watch event must include data"
    );
    let v = event.data.unwrap();
    assert_eq!(v.get_i64("phase3_watch.x"), Some(1));
}

#[test]
fn status_row_exposes_lifecycle_fields() {
    // Build a CacheRow directly from the wire-format JSON to test the new fields
    // without needing a live daemon with a lifecycle provider registered.
    use libbeachcomber::{CacheRow, RowKind};

    let wire = serde_json::json!({
        "provider": "git",
        "field": "branch",
        "path": "/tmp",
        "value": "main",
        "age_ms": 100u64,
        "stale": false,
        "kind": {"kind": "lifecycle", "decay": 0, "watches_files": true},
        "poll_interval_secs": 5u64,
        "keep_alive_polls": 3u32,
        "fsevents_reinstate": false,
        "failure": {"consecutive_failures": 0}
    });

    let row = CacheRow::from_wire(&wire).expect("from_wire");
    assert!(row.kind.is_some(), "kind must be populated");
    match row.kind.unwrap() {
        RowKind::Lifecycle {
            decay,
            watches_files,
        } => {
            assert_eq!(decay, 0);
            assert!(watches_files);
        }
        other => panic!("expected Lifecycle, got {:?}", other),
    }
    assert_eq!(row.poll_interval_secs, Some(5));
    assert_eq!(row.keep_alive_polls, Some(3));
    assert_eq!(row.fsevents_reinstate, Some(false));
    assert!(row.failure.is_some());
    assert_eq!(row.failure.unwrap().consecutive_failures, 0);
}
