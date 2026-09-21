//! Facet key syntax (`provider:facet`), checked at the request boundary in
//! `handle_request`/`handle_watch` via `query::parse_facet`. See
//! `docs/canon/field_resolution.md` "Facet" and
//! `docs/canon/provider_source.md` "Cache entry" for the canon this pins.

use beachcomber::cache::Cache;
use beachcomber::protocol::Response;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::server::Server;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

fn setup() -> (
    TempDir,
    std::path::PathBuf,
    Arc<Cache>,
    Arc<ProviderRegistry>,
    Arc<beachcomber::watcher_registry::WatcherRegistry>,
) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(ProviderRegistry::with_defaults());
    (tmp, sock, cache, registry, watchers)
}

async fn send_recv(stream: &mut UnixStream, request: &str) -> Response {
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    let mut reader = BufReader::new(&mut *stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    serde_json::from_str(&line).unwrap()
}

// (a) + (b): a facet key (`provider:facet`) round-trips through `put`/`get`
// exactly like any other static-namespace key — the colon is just part of the
// provider name.

#[tokio::test]
async fn put_then_get_field_on_facet_key() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    let put = send_recv(
        &mut stream,
        r#"{"op":"put","key":"myapp:notes","data":{"x":1}}"#,
    )
    .await;
    assert!(put.ok, "put to a facet key should succeed: {:?}", put.error);

    let get = send_recv(&mut stream, r#"{"op":"get","key":"myapp:notes.x"}"#).await;
    assert!(
        get.ok,
        "get of a facet field should succeed: {:?}",
        get.error
    );
    assert_eq!(get.data.unwrap(), 1);

    handle.abort();
}

#[tokio::test]
async fn get_whole_facet_key_returns_object() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    send_recv(
        &mut stream,
        r#"{"op":"put","key":"myapp:notes","data":{"x":1}}"#,
    )
    .await;

    let get = send_recv(&mut stream, r#"{"op":"get","key":"myapp:notes"}"#).await;
    assert!(
        get.ok,
        "get of the whole facet should succeed: {:?}",
        get.error
    );
    assert_eq!(get.data.unwrap(), serde_json::json!({"x": 1}));

    handle.abort();
}

// (c), (d), (e): malformed facet syntax is rejected with an exact
// "invalid facet key: <provider>" error before any provider lookup happens.

#[tokio::test]
async fn get_two_colons_is_invalid_facet() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let resp = send_recv(&mut stream, r#"{"op":"get","key":"a:b:c.x"}"#).await;
    assert!(!resp.ok, "two colons should be rejected as a facet key");
    assert_eq!(
        resp.error.as_deref(),
        Some("invalid facet key: a:b:c"),
        "got: {:?}",
        resp.error
    );

    handle.abort();
}

#[tokio::test]
async fn get_empty_facet_name_is_invalid() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let resp = send_recv(&mut stream, r#"{"op":"get","key":"a:.x"}"#).await;
    assert!(!resp.ok, "an empty facet name should be rejected");
    assert_eq!(
        resp.error.as_deref(),
        Some("invalid facet key: a:"),
        "got: {:?}",
        resp.error
    );

    handle.abort();
}

#[tokio::test]
async fn get_facet_name_with_bad_chars_is_invalid() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let resp = send_recv(&mut stream, r#"{"op":"get","key":"a:b!.x"}"#).await;
    assert!(
        !resp.ok,
        "a facet name with invalid characters should be rejected"
    );
    assert_eq!(
        resp.error.as_deref(),
        Some("invalid facet key: a:b!"),
        "got: {:?}",
        resp.error
    );

    handle.abort();
}

// `watch` checks facet syntax before it ever subscribes — the malformed key
// never reaches the subscription/initial-snapshot machinery.

#[tokio::test]
async fn watch_two_colons_is_invalid_facet_before_subscribing() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    stream
        .write_all(b"{\"op\":\"watch\",\"key\":\"a:b:c.x\"}\n")
        .await
        .unwrap();

    let mut reader = BufReader::new(&mut stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let resp: Response = serde_json::from_str(line.trim()).unwrap();

    assert!(!resp.ok, "watch on an invalid facet key should error");
    assert_eq!(
        resp.error.as_deref(),
        Some("invalid facet key: a:b:c"),
        "got: {:?}",
        resp.error
    );

    handle.abort();
}

// `refresh` shares the same two rules as `get`: facet syntax is checked (and
// rejected exactly the same way), and a metadata suffix is stripped before
// the facet check ever sees the provider segment.

#[tokio::test]
async fn refresh_two_colons_is_invalid_facet() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let resp = send_recv(&mut stream, r#"{"op":"refresh","key":"a:b:c"}"#).await;
    assert!(!resp.ok, "refresh on an invalid facet key should error");
    assert_eq!(
        resp.error.as_deref(),
        Some("invalid facet key: a:b:c"),
        "got: {:?}",
        resp.error
    );

    handle.abort();
}

#[tokio::test]
async fn refresh_on_a_facet_key_is_an_error() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let resp = send_recv(
        &mut stream,
        r#"{"op":"put","key":"x:template","data":{"a":"{{ 1 }}"}}"#,
    )
    .await;
    assert!(resp.ok, "put at the facet: {:?}", resp.error);

    // The facet key really is registered — an unregistered one would give the
    // same "no sources" message below and prove nothing about facets.
    let resp = send_recv(&mut stream, r#"{"op":"get","key":"x:template.a"}"#).await;
    assert_eq!(
        resp.data,
        Some(serde_json::json!("{{ 1 }}")),
        "the put must have landed: {:?}",
        resp.error
    );

    // A facet holds static records only. Reading configuration afresh is the
    // config watcher's job, not a refresh's.
    let resp = send_recv(&mut stream, r#"{"op":"refresh","key":"x:template"}"#).await;
    assert!(!resp.ok, "refresh on a facet key should error");
    assert_eq!(
        resp.error.as_deref(),
        Some("nothing to refresh: x:template has no sources"),
        "got: {:?}",
        resp.error
    );

    handle.abort();
}

#[tokio::test]
async fn refresh_metadata_suffix_wins_over_facet_parsing() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let resp = send_recv(&mut stream, r#"{"op":"refresh","key":"hostname:age"}"#).await;
    assert!(
        resp.ok,
        "hostname:age must be read as provider 'hostname' + suffix ':age', not a facet error: {:?}",
        resp.error
    );

    // `ok` alone would also be true of a refresh that did nothing. The suffix
    // was stripped and `hostname` refreshed only if its source actually ran, so
    // the cache — untouched until now — must hold its records.
    let resp = send_recv(&mut stream, r#"{"op":"status"}"#).await;
    let rows = resp.data.expect("status returns rows");
    assert!(
        rows.as_array()
            .expect("status returns an array")
            .iter()
            .any(|row| row["provider"] == "hostname"),
        "the refresh must have re-executed hostname's source, got {rows}"
    );

    handle.abort();
}

// (f): the five metadata-suffix names are unreachable as facets — a suffix
// strip always claims them first, so `hostname:age` is provider `hostname`
// plus `:age`, never a facet-syntax error. If suffix stripping did not run
// before the facet check, `parse_facet` would reject `age` as a facet name.

#[tokio::test]
async fn get_metadata_suffix_wins_over_facet_parsing() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let resp = send_recv(&mut stream, r#"{"op":"get","key":"hostname:age"}"#).await;
    assert!(
        resp.ok,
        "hostname:age must be read as provider 'hostname' + suffix ':age', not a facet error: {:?}",
        resp.error
    );
    // A miss is `ok` too, and answers `:age` with `null`. An age in
    // milliseconds is what says the suffix was read against a real namespace.
    assert!(
        resp.data.as_ref().is_some_and(|d| d.is_number()),
        "an age in milliseconds, got {:?}",
        resp.data
    );

    handle.abort();
}

// (g): `put` has no metadata-suffix handling, so `a:age` is always parsed as
// a facet candidate there — and rejected, since `age` is a metadata suffix name.

#[tokio::test]
async fn put_to_metadata_suffix_name_is_invalid_facet() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let resp = send_recv(&mut stream, r#"{"op":"put","key":"a:age","data":{"x":1}}"#).await;
    assert!(!resp.ok, "put to a:age should be rejected");
    assert_eq!(
        resp.error.as_deref(),
        Some("invalid facet key: a:age"),
        "got: {:?}",
        resp.error
    );

    handle.abort();
}

// (h): a facet key's `put`-written row shows up in `status` as a static row —
// source "put", kind "static" — exactly like any other static-namespace entry.

#[tokio::test]
async fn status_row_for_facet_key_put_is_static() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    let put = send_recv(
        &mut stream,
        r#"{"op":"put","key":"myapp:notes","data":{"x":1}}"#,
    )
    .await;
    assert!(put.ok, "put should succeed: {:?}", put.error);

    let status = send_recv(&mut stream, r#"{"op":"status"}"#).await;
    assert!(status.ok, "status should succeed: {:?}", status.error);
    let rows = status.data.expect("status must return row data");
    let rows = rows.as_array().expect("status data must be an array");

    let row = rows
        .iter()
        .find(|r| r["provider"] == "myapp:notes")
        .expect("a status row for the facet key must be present");
    assert_eq!(row["source"], "put", "row source must be 'put', got {row}");
    assert_eq!(
        row["kind"]["kind"], "static",
        "row kind must be 'static', got {row}"
    );

    handle.abort();
}
