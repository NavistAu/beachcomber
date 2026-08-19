// Regression coverage for methods that read the daemon's response line but
// must not swallow `{"ok": false, "error": "..."}` into a bare `Ok(())`.
//
// `put_null` is exercised against a real in-process daemon (DaemonGuard):
// asking it to `put --null` a builtin provider name is a genuine rejection
// (the daemon refuses to let a virtual put shadow a real provider). `refresh`
// and `set_context` have no such real rejection path today — the daemon
// always answers `ok: true` for them — so those are exercised against a
// one-shot fake responder that plays back a scripted `ok: false` line, the
// same raw-`UnixListener` double already used in connect_retry.rs. Either
// way, the point is the same: the client must surface the failure instead of
// discarding the response body.

mod common;
use common::daemon::DaemonGuard;

use libbeachcomber::{Client, ClientConfig, CombError};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::time::Duration;

fn client_for(sock: &std::path::Path) -> Client {
    Client::with_config(ClientConfig {
        timeout: Duration::from_secs(2),
        auto_start: false,
    })
    .with_socket_path(sock.to_path_buf())
}

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

#[test]
fn client_put_null_on_builtin_provider_returns_err() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);

    let result = client.put_null("hostname", None);

    assert!(
        result.is_err(),
        "put --null on a builtin provider should be rejected, got {result:?}"
    );
    assert!(
        matches!(result, Err(CombError::ServerError(_))),
        "expected ServerError, got {result:?}"
    );
}

#[test]
fn session_put_null_on_builtin_provider_returns_err() {
    let guard = DaemonGuard::spawn();
    let client = client_for(&guard.path);
    let mut session = client.session().expect("session");

    let result = session.put_null("hostname", None);

    assert!(
        result.is_err(),
        "put --null on a builtin provider should be rejected, got {result:?}"
    );
    assert!(
        matches!(result, Err(CombError::ServerError(_))),
        "expected ServerError, got {result:?}"
    );
}

#[test]
fn client_refresh_reports_daemon_rejection() {
    let (_tmp, sock) = spawn_canned_responder(r#"{"ok":false,"error":"unknown provider: nope"}"#);
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
    let (_tmp, sock) = spawn_canned_responder(r#"{"ok":false,"error":"unknown provider: nope"}"#);
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

#[test]
fn session_set_context_reports_daemon_rejection() {
    let (_tmp, sock) = spawn_canned_responder(r#"{"ok":false,"error":"bad path"}"#);
    let client = client_for(&sock);
    let mut session = client.session().expect("session");

    let result = session.set_context("/nonexistent");

    assert!(
        result.is_err(),
        "set_context must surface a daemon-side rejection, got {result:?}"
    );
    assert!(
        matches!(result, Err(CombError::ServerError(_))),
        "expected ServerError, got {result:?}"
    );
}
