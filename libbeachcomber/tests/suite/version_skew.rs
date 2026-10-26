// Task 3.8 — version-skew detection.
//
// The shared client compares the daemon's `hello`-reported version against
// its own build identity (`libbeachcomber::VERSION`) once per connection.
// A mismatch is not fatal — ops still succeed — but it must be surfaced via
// a caller-readable `version_skew()` and named in any subsequent op error.
//
// A fake daemon (the same raw-`UnixListener` double pattern used in
// connect_retry.rs and response_ok_checking.rs) answers `hello` with a
// scripted `daemon_version`, letting the mismatch be forced without any
// production code reading ambient environment.

use crate::common::client::client_for;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;

/// Bind a socket and, on the first connection, answer every `hello` request
/// with `daemon_version` and every other op with a scripted envelope
/// (`ok:true` unless `fail_ops` is set, in which case a daemon-side
/// rejection). Serves requests in a loop on the one connection, exactly as
/// a real `Session` (or a `Client`'s single-op connection) would see.
fn spawn_fake_daemon(daemon_version: &'static str, fail_ops: bool) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("sock");
    let listener = UnixListener::bind(&sock).unwrap();
    std::thread::spawn(move || {
        // A watch opens connections of its own beside the client's.
        while let Ok((stream, _)) = listener.accept() {
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut writer = stream;
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    let req: serde_json::Value = match serde_json::from_str(line.trim()) {
                        Ok(v) => v,
                        Err(_) => break,
                    };
                    let op = req.get("op").and_then(|v| v.as_str()).unwrap_or("");
                    let resp = if op == "hello" {
                        serde_json::json!({
                            "ok": true,
                            "data": {
                                "protocol_version": "1",
                                "daemon_version": daemon_version,
                            }
                        })
                    } else if fail_ops {
                        serde_json::json!({"ok": false, "error": "boom"})
                    } else {
                        serde_json::json!({"ok": true, "data": {"hostname": "fake"}})
                    };
                    if writer.write_all(format!("{resp}\n").as_bytes()).is_err() {
                        break;
                    }
                }
            });
        }
    });
    (tmp, sock)
}

#[test]
fn client_detects_mismatched_daemon_version_after_op_succeeds() {
    let (_tmp, sock) = spawn_fake_daemon("9.9.9-totally-different", false);
    let client = client_for(&sock);

    assert!(
        client.version_skew().is_none(),
        "skew must be unknown before any connection is made"
    );

    let result = client.get("hostname", None);
    assert!(
        result.is_ok(),
        "op must still succeed despite version skew: {result:?}"
    );

    let skew = client
        .version_skew()
        .expect("skew must be detectable after the first connection");
    assert_eq!(skew.ours, libbeachcomber::VERSION);
    assert_eq!(skew.theirs, "9.9.9-totally-different");
}

#[test]
fn client_reports_no_skew_when_versions_match() {
    let (_tmp, sock) = spawn_fake_daemon(libbeachcomber::VERSION, false);
    let client = client_for(&sock);

    let result = client.get("hostname", None);
    assert!(result.is_ok(), "op must succeed: {result:?}");
    assert!(
        client.version_skew().is_none(),
        "matching versions must not report skew"
    );
}

#[test]
fn client_subsequent_error_names_both_versions() {
    let (_tmp, sock) = spawn_fake_daemon("9.9.9-totally-different", true);
    let client = client_for(&sock);

    let result = client.refresh("nope", None);
    let err = result.expect_err("daemon-side rejection must surface as an error");
    let msg = err.to_string();
    assert!(
        msg.contains(libbeachcomber::VERSION),
        "error must name this client's version: {msg}"
    );
    assert!(
        msg.contains("9.9.9-totally-different"),
        "error must name the daemon's version: {msg}"
    );
}

#[test]
fn session_detects_mismatched_daemon_version_after_op_succeeds() {
    let (_tmp, sock) = spawn_fake_daemon("9.9.9-totally-different", false);
    let client = client_for(&sock);
    let mut session = client.session().expect("session");

    assert!(
        session.version_skew().is_none(),
        "skew must be unknown before the first op"
    );

    let result = session.get("hostname", None);
    assert!(
        result.is_ok(),
        "op must still succeed despite version skew: {result:?}"
    );

    let skew = session
        .version_skew()
        .expect("skew must be detectable after the first op")
        .clone();
    assert_eq!(skew.ours, libbeachcomber::VERSION);
    assert_eq!(skew.theirs, "9.9.9-totally-different");

    // A second op on the same connection must not re-probe (no crash, no
    // change in the reported skew) — the check is once per connection.
    let result2 = session.get("hostname", None);
    assert!(result2.is_ok());
    assert_eq!(session.version_skew(), Some(&skew));
}

#[test]
fn session_reports_no_skew_when_versions_match() {
    let (_tmp, sock) = spawn_fake_daemon(libbeachcomber::VERSION, false);
    let client = client_for(&sock);
    let mut session = client.session().expect("session");

    let result = session.get("hostname", None);
    assert!(result.is_ok(), "op must succeed: {result:?}");
    assert!(
        session.version_skew().is_none(),
        "matching versions must not report skew"
    );
}

#[test]
fn session_subsequent_error_names_both_versions() {
    let (_tmp, sock) = spawn_fake_daemon("9.9.9-totally-different", true);
    let client = client_for(&sock);
    let mut session = client.session().expect("session");

    let result = session.refresh("nope", None);
    let err = result.expect_err("daemon-side rejection must surface as an error");
    let msg = err.to_string();
    assert!(
        msg.contains(libbeachcomber::VERSION),
        "error must name this client's version: {msg}"
    );
    assert!(
        msg.contains("9.9.9-totally-different"),
        "error must name the daemon's version: {msg}"
    );
}

/// A watch's refusal of a key names the skew as an op's does: `comb watch`
/// and `comb get` print the same note.
#[test]
fn watch_refusal_names_both_versions() {
    use libbeachcomber::resolve::{Inputs, Slot};
    use libbeachcomber::watch::{ResolvedWatch, WatchOutcome};

    let (_tmp, sock) = spawn_fake_daemon("9.9.9-totally-different", true);
    let client = client_for(&sock);
    let env = std::collections::HashMap::new();
    let inputs = Inputs {
        cwd: "/",
        env: &env,
        slot: Slot::Global,
        force: false,
        wait: libbeachcomber::Wait::Budget,
        allow_exec: false,
        now: std::time::SystemTime::now(),
    };
    // A facet key is one plain daemon watch: its first frame is the refusal.
    let mut watch = ResolvedWatch::open(&client, "nope:cache", &inputs).expect("opens");
    let err = match watch.next(Some(std::time::Duration::from_secs(2))) {
        Err(e) => e.to_string(),
        Ok(WatchOutcome::Value(v)) => panic!("a refusal, got {v:?}"),
        Ok(other) => panic!("a refusal, got {other:?}"),
    };
    assert!(err.contains("boom"), "{err}");
    assert!(err.contains(libbeachcomber::VERSION), "{err}");
    assert!(err.contains("9.9.9-totally-different"), "{err}");
}

/// The Rust `Client::watch`'s `next_event` names the skew in a refusal too:
/// the same note as `comb watch` and `comb get`.
#[test]
fn client_watch_refusal_names_both_versions() {
    let (_tmp, sock) = spawn_fake_daemon("9.9.9-totally-different", true);
    let client = client_for(&sock);
    let mut stream = client.watch("nope", None).expect("opens");
    let err = stream
        .next_event()
        .expect_err("a refusal, not an event")
        .to_string();
    assert!(err.contains("boom"), "{err}");
    assert!(err.contains(libbeachcomber::VERSION), "{err}");
    assert!(err.contains("9.9.9-totally-different"), "{err}");
}
