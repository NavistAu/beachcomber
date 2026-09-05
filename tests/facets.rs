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
// exactly like any other dynamic-provider key — the colon is just part of the
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

// (c), (d), (e): malformed facet syntax is rejected with "invalid facet key"
// before any provider lookup happens.

#[tokio::test]
async fn get_two_colons_is_invalid_facet() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let resp = send_recv(&mut stream, r#"{"op":"get","key":"a:b:c.x"}"#).await;
    assert!(!resp.ok, "two colons should be rejected as a facet key");
    assert!(
        resp.error
            .as_deref()
            .unwrap_or("")
            .contains("invalid facet key"),
        "expected 'invalid facet key' error, got: {:?}",
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
    assert!(
        resp.error
            .as_deref()
            .unwrap_or("")
            .contains("invalid facet key"),
        "expected 'invalid facet key' error, got: {:?}",
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
    assert!(
        resp.error
            .as_deref()
            .unwrap_or("")
            .contains("invalid facet key"),
        "expected 'invalid facet key' error, got: {:?}",
        resp.error
    );

    handle.abort();
}

// (f): the five metadata-suffix names are unreachable as facets — a suffix
// strip always claims them first, so `a:age` is provider `a` plus `:age`,
// never a facet-syntax error.

#[tokio::test]
async fn metadata_suffix_wins_over_facet_parsing() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    // Register "a" as a dynamic provider, then clear its cache entry so the
    // provider is known but the entry is a miss.
    let put = send_recv(&mut stream, r#"{"op":"put","key":"a","data":{"seed":1}}"#).await;
    assert!(put.ok, "seeding put should succeed: {:?}", put.error);
    let clear = send_recv(&mut stream, r#"{"op":"put","key":"a"}"#).await;
    assert!(clear.ok, "null-data put should succeed: {:?}", clear.error);

    let resp = send_recv(&mut stream, r#"{"op":"get","key":"a:age"}"#).await;
    assert!(
        resp.ok,
        "a:age must be read as provider 'a' + suffix ':age', not a facet error: {:?}",
        resp.error
    );
    assert_eq!(
        resp.data, None,
        "age of a cache miss is null, wrapped in an ok response"
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
    assert!(
        resp.error
            .as_deref()
            .unwrap_or("")
            .contains("invalid facet key"),
        "expected 'invalid facet key' error, got: {:?}",
        resp.error
    );

    handle.abort();
}

// (h): a facet key's `put`-written row shows up in `status` as a static row —
// source "put", kind "static" — exactly like any other dynamic-provider entry.

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
