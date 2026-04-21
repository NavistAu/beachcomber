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

/// Open a fresh connection to the server socket, send a request, return the parsed response.
async fn send_one(sock: &std::path::Path, req: &str) -> Response {
    let mut stream = UnixStream::connect(sock).await.unwrap();
    stream
        .write_all(format!("{req}\n").as_bytes())
        .await
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    serde_json::from_str(line.trim()).unwrap()
}

/// Open a connection, send multiple requests in sequence, collect responses.
async fn send_many(sock: &std::path::Path, reqs: &[&str]) -> Vec<Response> {
    let mut stream = UnixStream::connect(sock).await.unwrap();
    for req in reqs {
        stream
            .write_all(format!("{req}\n").as_bytes())
            .await
            .unwrap();
    }
    let mut reader = BufReader::new(stream);
    let mut responses = Vec::new();
    for _ in reqs {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let r: Response = serde_json::from_str(line.trim()).unwrap();
        responses.push(r);
    }
    responses
}

// ── Test 1: :source returns "builtin" for a builtin provider ────────────────

#[tokio::test]
async fn source_returns_builtin_for_hostname() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let resp = send_one(&sock, r#"{"op":"get","key":"hostname:source"}"#).await;

    assert!(resp.ok, "expected ok response");
    assert_eq!(resp.data.unwrap(), "builtin");

    handle.abort();
}

// ── Test 2: :source returns "virtual" after a store ─────────────────────────

#[tokio::test]
async fn source_returns_virtual_after_store() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let resps = send_many(
        &sock,
        &[
            r#"{"op":"store","key":"myvirt","data":{"status":"ok"}}"#,
            r#"{"op":"get","key":"myvirt:source"}"#,
        ],
    )
    .await;

    assert!(resps[0].ok, "store should succeed");
    assert!(resps[1].ok, "source query should succeed");
    assert_eq!(resps[1].data.as_ref().unwrap(), "virtual");

    handle.abort();
}

// ── Test 3: :cache returns true after a warm get ─────────────────────────────

#[tokio::test]
async fn cache_returns_true_after_warm_get() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Warm the cache with an initial get (sync-miss executes inline and stores).
    let warm = send_one(&sock, r#"{"op":"get","key":"hostname"}"#).await;
    assert!(warm.ok, "warm get should succeed");

    // Now query :cache — should be true because the second get hits the warm cache.
    let resp = send_one(&sock, r#"{"op":"get","key":"hostname:cache"}"#).await;
    assert!(resp.ok, "cache suffix query should succeed");
    assert_eq!(
        resp.data.unwrap(),
        serde_json::Value::Bool(true),
        ":cache should be true after warming"
    );

    handle.abort();
}

// ── Test 4: :fresh is the inverse of :stale ──────────────────────────────────

#[tokio::test]
async fn fresh_inverts_stale() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Warm first.
    let warm = send_one(&sock, r#"{"op":"get","key":"hostname"}"#).await;
    assert!(warm.ok);

    let resps = send_many(
        &sock,
        &[
            r#"{"op":"get","key":"hostname:stale"}"#,
            r#"{"op":"get","key":"hostname:fresh"}"#,
        ],
    )
    .await;

    assert!(resps[0].ok, "stale query should succeed");
    assert!(resps[1].ok, "fresh query should succeed");

    let stale = resps[0].data.as_ref().unwrap().as_bool().unwrap();
    let fresh = resps[1].data.as_ref().unwrap().as_bool().unwrap();
    assert_eq!(fresh, !stale, ":fresh must be the inverse of :stale");

    handle.abort();
}

// ── Test 5: :age returns a numeric string ────────────────────────────────────

#[tokio::test]
async fn age_returns_numeric_string() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Warm first.
    let warm = send_one(&sock, r#"{"op":"get","key":"hostname"}"#).await;
    assert!(warm.ok);

    let resp = send_one(&sock, r#"{"op":"get","key":"hostname:age"}"#).await;
    assert!(resp.ok, "age query should succeed");

    let age_str = resp
        .data
        .as_ref()
        .unwrap()
        .as_str()
        .expect(":age data should be a string");
    age_str
        .parse::<u128>()
        .expect(":age string should parse as u128");

    handle.abort();
}

// ── Test 6: unknown suffix is passed through as a field name ─────────────────

#[tokio::test]
async fn unknown_suffix_is_passed_through_as_field() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Warm first so it's definitely in cache.
    let warm = send_one(&sock, r#"{"op":"get","key":"hostname"}"#).await;
    assert!(warm.ok);

    // "bogus" is not a known metadata suffix, so the server treats the whole key
    // as "hostname:bogus" where ":bogus" is NOT stripped — but protocol::split_key
    // splits on "." not ":", so provider_name becomes "hostname:bogus", which is
    // an unknown provider. That means we get an "unknown provider" error.
    let resp = send_one(&sock, r#"{"op":"get","key":"hostname:bogus"}"#).await;
    assert!(!resp.ok, "unknown suffix should not be treated as metadata");
    let err = resp.error.unwrap();
    assert!(
        err.contains("unknown provider"),
        "expected 'unknown provider' error, got: {err}"
    );

    handle.abort();
}
