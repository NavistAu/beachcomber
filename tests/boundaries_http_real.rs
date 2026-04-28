//! Real-implementation tests for `UreqHttpFetcher` using a mockito HTTP server.
//!
//! These tests exercise the actual ureq-backed implementation (not the `StubFetcher`
//! used in `provider_http_seams.rs`) to push `src/boundaries/http.rs` coverage
//! from 0% to 75%+.
//!
//! Coverage paths exercised:
//!   - GET request happy path (200)
//!   - GET request with 404 (ureq 3 treats 4xx as Err by default)
//!   - POST with request body
//!   - PUT with request body
//!   - PATCH with request body
//!   - Custom request headers propagated to server
//!   - Response headers returned in HttpResponse
//!   - Timeout: fetcher returns Err when server is slow
//!   - Connection refused: fetcher returns Err

use beachcomber::boundaries::http::{HttpFetcher, UreqHttpFetcher};
use mockito::Server;
use std::time::Duration;

fn fetcher() -> UreqHttpFetcher {
    UreqHttpFetcher
}

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
    let result = fetcher().fetch("GET".to_string(), url, vec![], None, Duration::from_secs(5));

    let resp = result.expect("GET 200 must succeed");
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, b"hello world");
}

// ── GET 404 returns Err (ureq 3 behaviour) ───────────────────────────────────

/// ureq 3 treats 4xx responses as errors by default.
/// Verify that `UreqHttpFetcher::fetch` returns `Err` for a 404 response.
#[test]
fn get_404_returns_err() {
    let mut server = Server::new();
    let _mock = server
        .mock("GET", "/missing")
        .with_status(404)
        .with_body("not found")
        .create();

    let url = format!("{}/missing", server.url());
    let result = fetcher().fetch("GET".to_string(), url, vec![], None, Duration::from_secs(5));

    assert!(
        result.is_err(),
        "ureq 3 should treat 404 as Err, got: {:?}",
        result
    );
}

// ── GET 500 returns Err (ureq 3 behaviour) ───────────────────────────────────

/// ureq 3 treats 5xx responses as errors by default.
#[test]
fn get_500_returns_err() {
    let mut server = Server::new();
    let _mock = server
        .mock("GET", "/boom")
        .with_status(500)
        .with_body("internal server error")
        .create();

    let url = format!("{}/boom", server.url());
    let result = fetcher().fetch("GET".to_string(), url, vec![], None, Duration::from_secs(5));

    assert!(
        result.is_err(),
        "ureq 3 should treat 500 as Err, got: {:?}",
        result
    );
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
    let result = fetcher().fetch(
        "POST".to_string(),
        url,
        vec![("content-type".to_string(), "application/json".to_string())],
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
    let result = fetcher().fetch(
        "PUT".to_string(),
        url,
        vec![],
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
    let result = fetcher().fetch(
        "PATCH".to_string(),
        url,
        vec![],
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
    let result = fetcher().fetch(
        "GET".to_string(),
        url,
        vec![("x-api-key".to_string(), "secret123".to_string())],
        None,
        Duration::from_secs(5),
    );

    let resp = result.expect("request with matching header must succeed");
    assert_eq!(resp.status, 200);
}

// ── Response headers returned in HttpResponse ─────────────────────────────────

/// Response headers from the server must appear in `HttpResponse.headers`.
#[test]
fn response_headers_are_propagated() {
    let mut server = Server::new();
    let _mock = server
        .mock("GET", "/headers")
        .with_status(200)
        .with_header("x-custom-header", "myvalue")
        .with_body("")
        .create();

    let url = format!("{}/headers", server.url());
    let result = fetcher().fetch("GET".to_string(), url, vec![], None, Duration::from_secs(5));

    let resp = result.expect("GET must succeed");
    let found = resp
        .headers
        .iter()
        .any(|(k, v)| k.to_lowercase() == "x-custom-header" && v == "myvalue");
    assert!(
        found,
        "response headers must include x-custom-header=myvalue, got: {:?}",
        resp.headers
    );
}

// ── Timeout ───────────────────────────────────────────────────────────────────

/// When the server delays beyond the fetcher timeout, fetch must return Err.
#[test]
fn timeout_returns_err() {
    let mut server = Server::new();
    let _mock = server
        .mock("GET", "/slow")
        .with_status(200)
        .with_body("too late")
        .with_chunked_body(|_| {
            // Delay the response body by sleeping much longer than our timeout.
            std::thread::sleep(Duration::from_millis(600));
            Ok(())
        })
        .create();

    let url = format!("{}/slow", server.url());
    let result = fetcher().fetch(
        "GET".to_string(),
        url,
        vec![],
        None,
        Duration::from_millis(100),
    );

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
    let result = fetcher().fetch("GET".to_string(), url, vec![], None, Duration::from_secs(2));

    assert!(
        result.is_err(),
        "connection refused must return Err, got: {:?}",
        result
    );
}
