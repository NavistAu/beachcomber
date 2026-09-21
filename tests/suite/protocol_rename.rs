use beachcomber::protocol::Request;
use serde_json::json;

#[test]
fn refresh_request_deserializes_from_refresh_op() {
    let payload = json!({"op": "refresh", "key": "git", "path": null});
    let req: Request = serde_json::from_value(payload).unwrap();
    match req {
        Request::Refresh { key, path } => {
            assert_eq!(key, "git");
            assert_eq!(path, None);
        }
        _ => panic!("expected Refresh variant"),
    }
}

#[test]
fn put_request_deserializes_from_put_op() {
    let payload = serde_json::json!({"op": "put", "key": "k", "data": {"a": 1}});
    let req: beachcomber::protocol::Request = serde_json::from_value(payload).unwrap();
    assert!(matches!(req, beachcomber::protocol::Request::Put { .. }));
}

#[test]
fn introspect_request_deserializes_daemon_subject() {
    let payload = serde_json::json!({"op": "introspect", "subject": "daemon"});
    let req: beachcomber::protocol::Request = serde_json::from_value(payload).unwrap();
    match req {
        beachcomber::protocol::Request::Introspect { subject, .. } => {
            assert_eq!(subject, beachcomber::protocol::IntrospectSubject::Daemon);
        }
        _ => panic!("expected Introspect variant"),
    }
}

#[test]
fn introspect_request_deserializes_all_subjects() {
    use beachcomber::protocol::{IntrospectSubject, Request};
    for (wire, expected) in &[
        ("daemon", IntrospectSubject::Daemon),
        ("providers", IntrospectSubject::Providers),
        ("config", IntrospectSubject::Config),
        ("cache", IntrospectSubject::Cache),
        ("lifecycle", IntrospectSubject::Lifecycle),
        ("watches", IntrospectSubject::Watches),
        ("timers", IntrospectSubject::Timers),
        ("demand", IntrospectSubject::Demand),
        ("procs", IntrospectSubject::Procs),
    ] {
        let payload = serde_json::json!({"op": "introspect", "subject": wire});
        let req: Request = serde_json::from_value(payload).unwrap();
        match req {
            Request::Introspect { subject, .. } => assert_eq!(subject, *expected),
            _ => panic!("expected Introspect variant for subject {wire}"),
        }
    }
}
