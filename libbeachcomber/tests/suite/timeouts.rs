//! A daemon that does not answer within the client's timeout is
//! `CombError::Timeout` on every op, on a `Client` and on a `Session` alike —
//! one request path, one classification — and a resolution over either keeps
//! that kind.

use libbeachcomber::resolve::{self, Inputs, Slot};
use libbeachcomber::{Client, ClientConfig, CombError, IntrospectSubject};
use std::collections::HashMap;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::time::Duration;

/// A socket whose connections are accepted, held open and never answered.
fn silent_daemon() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("sock");
    let listener = UnixListener::bind(&sock).unwrap();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            held.push(stream);
        }
    });
    (tmp, sock)
}

fn impatient_client(sock: &std::path::Path) -> Client {
    Client::with_config(ClientConfig {
        timeout: Duration::from_millis(50),
        auto_start: false,
        exec: false,
    })
    .with_socket_path(sock.to_path_buf())
}

fn assert_timeout<T: std::fmt::Debug>(op: &str, r: Result<T, CombError>) {
    match r {
        Err(e @ CombError::Timeout) => assert_eq!(e.kind(), "timeout"),
        other => panic!("{op}: expected Timeout, got {other:?}"),
    }
}

#[test]
fn every_client_op_that_waits_too_long_is_a_timeout() {
    let (_tmp, sock) = silent_daemon();
    let client = impatient_client(&sock);

    assert_timeout("get", client.get("p.x", None));
    assert_timeout("get_node", client.get_node("p.x", None, false, false));
    assert_timeout(
        "put",
        client.put("p", serde_json::json!({"x": 1}), None, None),
    );
    assert_timeout("refresh", client.refresh("p", None));
    assert_timeout("status", client.status());
    assert_timeout(
        "introspect",
        client.introspect(IntrospectSubject::Daemon, None),
    );
    assert_timeout("hello", client.hello());
}

#[test]
fn every_session_op_that_waits_too_long_is_a_timeout() {
    let (_tmp, sock) = silent_daemon();
    let client = impatient_client(&sock);
    let mut session = client.session().expect("connects");

    assert_timeout("get", session.get("p.x", None));
    assert_timeout(
        "put",
        session.put("p", serde_json::json!({"x": 1}), None, None),
    );
    assert_timeout("refresh", session.refresh("p", None));
    assert_timeout("status", session.status());
    assert_timeout("layers", session.layers("p", None));
    assert_timeout(
        "introspect",
        session.introspect(IntrospectSubject::Daemon, None),
    );
    assert_timeout("hello", session.hello());
}

/// `resolve` runs over a `Session` as it does over a `Client`, and a
/// transport failure reaches the caller as the `CombError` it was.
#[test]
fn a_resolution_keeps_the_transports_kind_over_a_client_and_a_session() {
    let (_tmp, sock) = silent_daemon();
    let client = impatient_client(&sock);
    let env = HashMap::new();
    let inputs = Inputs {
        cwd: "/",
        env: &env,
        slot: Slot::Global,
        force: false,
        wait: false,
        allow_exec: false,
        now: std::time::SystemTime::now(),
    };

    let mut over_client = &client;
    let err = resolve::resolve("p.x", &inputs, &mut over_client).expect_err("times out");
    assert_eq!(err.kind(), "timeout", "{err}");

    let mut session = client.session().expect("connects");
    let err = resolve::resolve("p.x", &inputs, &mut session).expect_err("times out");
    assert_eq!(err.kind(), "timeout", "{err}");
}
