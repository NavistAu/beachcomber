// Regression coverage for methods that read the daemon's response line but
// must not swallow `{"ok": false, "error": "..."}` into a bare `Ok(())`.
//
// a null `put` is exercised against a real in-process daemon (DaemonGuard):
// asking it to put a key nested past ten segments is a genuine
// rejection (`put key is nested too deeply`). `refresh` has no such real
// rejection path today — the daemon always answers `ok: true` for it — so it
// is exercised against a one-shot fake responder that plays back a scripted
// `ok: false` line, the
// same raw-`UnixListener` double already used in connect_retry.rs. Either
// way, the point is the same: the client must surface the failure instead of
// discarding the response body.

use crate::common::client::client_for;
use crate::common::daemon::DaemonGuard;

use libbeachcomber::CombError;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;

/// Bind a socket and, on the first connection, read one request line and
/// reply with a single scripted response line.
fn spawn_canned_responder(response: &'static str) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("sock");
    let listener = UnixListener::bind(&sock).unwrap();
    std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let mut stream = reader.into_inner();
            let _ = stream.write_all(format!("{response}\n").as_bytes());
        }
    });
    (tmp, sock)
}

/// A key nested past ten segments is refused by the daemon.
const TOO_DEEP: &str = "a.b.c.d.e.f.g.h.i.j.k.l.m";

#[test]
fn client_put_of_null_on_an_over_deep_key_returns_err() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);

    let result = client.put(TOO_DEEP, serde_json::Value::Null, None, None);

    assert!(
        result.is_err(),
        "put null on an over-deep key should be rejected, got {result:?}"
    );
    assert!(
        matches!(result, Err(CombError::ServerError(_))),
        "expected ServerError, got {result:?}"
    );
}

#[test]
fn session_put_of_null_on_an_over_deep_key_returns_err() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);
    let mut session = client.session().expect("session");

    let result = session.put(TOO_DEEP, serde_json::Value::Null, None, None);

    assert!(
        result.is_err(),
        "put null on an over-deep key should be rejected, got {result:?}"
    );
    assert!(
        matches!(result, Err(CombError::ServerError(_))),
        "expected ServerError, got {result:?}"
    );
}

#[test]
fn client_refresh_reports_daemon_rejection() {
    let (_tmp, sock) = spawn_canned_responder(r#"{"ok":false,"error":"invalid facet key: a:b:c"}"#);
    let client = client_for(&sock);

    let result = client.refresh("nope", None);

    assert!(
        result.is_err(),
        "refresh must surface a daemon-side rejection, got {result:?}"
    );
    assert!(
        matches!(result, Err(CombError::ServerError(_))),
        "expected ServerError, got {result:?}"
    );
}

#[test]
fn session_refresh_reports_daemon_rejection() {
    let (_tmp, sock) = spawn_canned_responder(r#"{"ok":false,"error":"invalid facet key: a:b:c"}"#);
    let client = client_for(&sock);
    let mut session = client.session().expect("session");

    let result = session.refresh("nope", None);

    assert!(
        result.is_err(),
        "refresh must surface a daemon-side rejection, got {result:?}"
    );
    assert!(
        matches!(result, Err(CombError::ServerError(_))),
        "expected ServerError, got {result:?}"
    );
}

/// A daemon that hangs up without answering is reported as that, not as a
/// reply that could not be parsed.
#[test]
fn a_dropped_connection_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("sock");
    let listener = UnixListener::bind(&sock).unwrap();
    std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let mut line = String::new();
            let _ = BufReader::new(stream).read_line(&mut line);
        }
    });
    let err = client_for(&sock).refresh("p", None).unwrap_err();
    assert!(
        matches!(&err, CombError::IoError(e) if e.kind() == std::io::ErrorKind::UnexpectedEof),
        "{err}"
    );
    assert!(err.to_string().contains("closed the connection"), "{err}");
}

/// `set_context` is the session's own: it sends nothing, so a daemon that
/// would refuse every request does not see it, and a path it cannot read is
/// the caller's error.
#[test]
fn session_set_context_is_client_side() {
    let (_tmp, sock) = spawn_canned_responder(r#"{"ok":false,"error":"bad path"}"#);
    let client = client_for(&sock);
    let mut session = client.session().expect("session");

    assert!(session.set_context("/nonexistent").is_ok());
    assert!(matches!(
        session.set_context("~bob/x"),
        Err(CombError::ParseError(m)) if m.contains("~bob/x")
    ));
}
