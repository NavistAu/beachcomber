//! Tests for `boundaries::http::fetch` against a mockito HTTP server: the
//! real ureq-backed request, which `url()` is the only caller of.
//!
//! Paths exercised:
//!   - GET happy path (200)
//!   - 4xx and 5xx returned as responses; the caller judges the status
//!   - POST, PUT and PATCH with a request body
//!   - Custom request headers propagated to the server
//!   - A method outside GET, POST, PUT and PATCH is an error and sends nothing
//!   - Timeout: an Err when the server is slow
//!   - Connection refused: an Err

use beachcomber::boundaries::http::fetch;
use mockito::Server;
use std::time::Duration;

// ── GET 200 with text body ────────────────────────────────────────────────────

/// GET 200 must return Ok with the correct status and body bytes.
#[test]
fn get_200_returns_ok_with_body() {
    let mut server = Server::new();
    let _mock = server
        .mock("GET", "/hello")
        .with_status(200)
        .with_header("content-type", "text/plain")
        .with_body("hello world")
        .create();

    let url = format!("{}/hello", server.url());
    let result = fetch("GET", &url, &[], None, Duration::from_secs(5));

    let resp = result.expect("GET 200 must succeed");
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, b"hello world");
}

// ── Non-2xx statuses are responses, not errors ───────────────────────────────

/// The fetcher reports 4xx and 5xx as ordinary responses; each caller judges
/// the status.
#[test]
fn get_404_returns_the_response() {
    let mut server = Server::new();
    let _mock = server
        .mock("GET", "/missing")
        .with_status(404)
        .with_body("not found")
        .create();

    let url = format!("{}/missing", server.url());
    let resp = fetch("GET", &url, &[], None, Duration::from_secs(5)).expect("a 404 is a response");

    assert_eq!(resp.status, 404);
    assert_eq!(resp.body, b"not found");
}

#[test]
fn get_500_returns_the_response() {
    let mut server = Server::new();
    let _mock = server
        .mock("GET", "/boom")
        .with_status(500)
        .with_body("internal server error")
        .create();

    let url = format!("{}/boom", server.url());
    let resp = fetch("GET", &url, &[], None, Duration::from_secs(5)).expect("a 500 is a response");

    assert_eq!(resp.status, 500);
}

// ── POST with JSON body ───────────────────────────────────────────────────────

/// POST must send the body bytes; server echoes them back so we can assert.
#[test]
fn post_with_body_sends_body() {
    let mut server = Server::new();
    let _mock = server
        .mock("POST", "/data")
        .match_body(r#"{"key":"value"}"#)
        .with_status(200)
        .with_body("accepted")
        .create();

    let url = format!("{}/data", server.url());
    let body = br#"{"key":"value"}"#.to_vec();
    let result = fetch(
        "POST",
        &url,
        &[("content-type".to_string(), "application/json".to_string())],
        Some(body),
        Duration::from_secs(5),
    );

    let resp = result.expect("POST with body must succeed");
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, b"accepted");
}

// ── PUT with body ─────────────────────────────────────────────────────────────

/// PUT must reach the correct HTTP method branch in the implementation.
#[test]
fn put_with_body_succeeds() {
    let mut server = Server::new();
    let _mock = server
        .mock("PUT", "/resource")
        .with_status(200)
        .with_body("updated")
        .create();

    let url = format!("{}/resource", server.url());
    let result = fetch(
        "PUT",
        &url,
        &[],
        Some(b"payload".to_vec()),
        Duration::from_secs(5),
    );

    let resp = result.expect("PUT must succeed");
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, b"updated");
}

// ── PATCH with body ───────────────────────────────────────────────────────────

/// PATCH must reach the correct HTTP method branch in the implementation.
#[test]
fn patch_with_body_succeeds() {
    let mut server = Server::new();
    let _mock = server
        .mock("PATCH", "/resource")
        .with_status(200)
        .with_body("patched")
        .create();

    let url = format!("{}/resource", server.url());
    let result = fetch(
        "PATCH",
        &url,
        &[],
        Some(b"delta".to_vec()),
        Duration::from_secs(5),
    );

    let resp = result.expect("PATCH must succeed");
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, b"patched");
}

// ── Custom request header propagated ─────────────────────────────────────────

/// A header supplied by the caller must reach the server.
#[test]
fn custom_request_header_is_sent() {
    let mut server = Server::new();
    let _mock = server
        .mock("GET", "/auth")
        .match_header("x-api-key", "secret123")
        .with_status(200)
        .with_body("ok")
        .create();

    let url = format!("{}/auth", server.url());
    let result = fetch(
        "GET",
        &url,
        &[("x-api-key".to_string(), "secret123".to_string())],
        None,
        Duration::from_secs(5),
    );

    let resp = result.expect("request with matching header must succeed");
    assert_eq!(resp.status, 200);
}

// ── Timeout ───────────────────────────────────────────────────────────────────

/// When the server delays beyond the fetcher timeout, fetch must return Err.
#[test]
fn timeout_returns_err() {
    // The handler withholds the body until the test releases it, rather than
    // guessing at a delay long enough to outlast the client timeout: `recv`
    // returns Err the moment `release` is dropped, so the server hangs for
    // exactly as long as this test needs it to and not a millisecond more.
    let (release, blocked) = std::sync::mpsc::channel::<()>();
    let blocked = std::sync::Mutex::new(blocked);

    let mut server = Server::new();
    let _mock = server
        .mock("GET", "/slow")
        .with_status(200)
        .with_body("too late")
        .with_chunked_body(move |_| {
            let _ = blocked.lock().unwrap().recv();
            Ok(())
        })
        .create();

    let url = format!("{}/slow", server.url());
    let result = fetch("GET", &url, &[], None, Duration::from_millis(20));

    // Unblock before asserting: an unwinding assert would otherwise drop the
    // server (which joins its handler) ahead of the sender.
    drop(release);

    assert!(
        result.is_err(),
        "fetch should time out and return Err, got: {:?}",
        result
    );
}

// ── Connection refused ────────────────────────────────────────────────────────

/// Connecting to a port that is not listening must return Err immediately.
#[test]
fn connection_refused_returns_err() {
    // Bind a listener to get an OS-assigned port, then drop it so the port is
    // no longer accepting connections.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind must succeed");
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let url = format!("http://127.0.0.1:{}/anything", port);
    let result = fetch("GET", &url, &[], None, Duration::from_secs(2));

    assert!(
        result.is_err(),
        "connection refused must return Err, got: {:?}",
        result
    );
}

// ── Unknown method ───────────────────────────────────────────────────────────

/// A method outside GET, POST, PUT and PATCH is an error; it is never sent as
/// a GET.
#[test]
fn an_unknown_method_is_an_error() {
    let mut server = Server::new();
    let mock = server
        .mock("GET", "/anything")
        .with_status(200)
        .expect(0)
        .create();

    let url = format!("{}/anything", server.url());
    let err = fetch("DELETE", &url, &[], None, Duration::from_secs(5))
        .expect_err("DELETE is not a supported method");

    assert!(err.contains("DELETE"), "the error names the method: {err}");
    mock.assert();
}
