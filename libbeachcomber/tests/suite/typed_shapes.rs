// Phase 3: typed-shape integration tests for the public SDK crate.

use crate::common::client::client_for;
use crate::common::daemon::DaemonGuard;

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
            false,
        )
        .expect("put");

    assert_eq!(
        client.get("phase3_put.color", None).expect("get"),
        Some(serde_json::json!("purple")),
        "put followed by get should hit"
    );
}

#[test]
fn watch_receives_initial_event() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);

    client
        .put(
            "phase3_watch",
            serde_json::json!({"x": 1}),
            None,
            None,
            false,
        )
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
        "source": "source(head)",
        "path": "/tmp",
        "value": "main",
        "age_ms": 100u64,
        "kind": {"kind": "lifecycle", "decay": 0, "depth": 4, "watches_files": true},
        "poll_interval_secs": 5u64,
        "poll_count": 3u32,
        "failure": {"consecutive_failures": 0}
    });

    let row: CacheRow = serde_json::from_value(wire).expect("a wire row");
    assert!(row.kind.is_some(), "kind must be populated");
    match row.kind.unwrap() {
        RowKind::Lifecycle {
            decay,
            watches_files,
            ..
        } => {
            assert_eq!(decay, 0);
            assert!(watches_files);
        }
        other => panic!("expected Lifecycle, got {:?}", other),
    }
    assert_eq!(row.poll_interval_secs, Some(5));
    assert_eq!(row.poll_count, Some(3));
    assert!(row.failure.is_some());
    assert_eq!(row.failure.unwrap().consecutive_failures, 0);
}
