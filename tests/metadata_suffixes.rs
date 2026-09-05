mod common;

use beachcomber::cache::Cache;
use beachcomber::protocol::Response;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::server::Server;
use common::git::GitRepoFixture;
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

// ── :source names the writer of the record that answered ────────────────────
//
// Not the kind of the namespace: a record has exactly one source, and that
// name — `host`, `head`, `put`, `config:<file>` — is what `:source` reports.

#[tokio::test]
async fn source_names_the_owning_source() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let resp = send_one(&sock, r#"{"op":"get","key":"hostname.name:source"}"#).await;

    assert!(resp.ok, "expected ok response");
    assert_eq!(resp.data.unwrap(), "host");

    handle.abort();
}

#[tokio::test]
async fn source_names_put_for_a_put_record() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let resps = send_many(
        &sock,
        &[
            r#"{"op":"put","key":"myvirt","data":{"a":"ok"}}"#,
            r#"{"op":"get","key":"myvirt.a:source"}"#,
        ],
    )
    .await;

    assert!(resps[0].ok, "put should succeed");
    assert!(resps[1].ok, "source query should succeed");
    assert_eq!(resps[1].data.as_ref().unwrap(), "put");

    handle.abort();
}

#[tokio::test]
async fn source_is_null_for_an_object_and_for_a_miss() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    // A whole namespace is assembled from several records and names no one
    // writer. `null` on the wire, which reads back as an absent `data` — the
    // shape `:age` already has on a miss.
    let resp = send_one(&sock, r#"{"op":"get","key":"hostname:source"}"#).await;
    assert!(resp.ok, "expected ok response: {:?}", resp.error);
    assert!(resp.data.is_none(), "got {:?}", resp.data);

    // A miss has no record to name either.
    let resp = send_one(&sock, r#"{"op":"get","key":"hostname.nonesuch:source"}"#).await;
    assert!(resp.ok, "expected ok response: {:?}", resp.error);
    assert!(resp.data.is_none(), "got {:?}", resp.data);

    handle.abort();
}

#[tokio::test]
async fn source_names_the_template_record_on_a_template_only_hit() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let resps = send_many(
        &sock,
        &[
            r#"{"op":"put","key":"scanline:template","data":{"widgets":{"foo":"{{ bar }}"}}}"#,
            r#"{"op":"get","key":"scanline.widgets.foo:source"}"#,
        ],
    )
    .await;

    assert!(resps[0].ok, "put should succeed: {:?}", resps[0].error);
    assert!(resps[1].ok, "source query should succeed");
    assert_eq!(
        resps[1].data.as_ref().unwrap(),
        "put",
        "a template-only hit is answered by the template record, and names its writer"
    );

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_names_the_source_behind_a_warm_provider_field() {
    if !common::git::has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let repo = GitRepoFixture::new();

    let req = serde_json::json!({"op": "get", "key": "git.branch:source", "path": repo.path_str()})
        .to_string();
    let resp = send_one(&sock, &req).await;
    assert!(resp.ok, "expected ok response: {:?}", resp.error);
    assert_eq!(resp.data.unwrap(), "head");

    handle.abort();
}

#[tokio::test]
async fn source_on_an_unknown_provider_is_the_plain_get_error() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let resp = send_one(&sock, r#"{"op":"get","key":"nope.x:source"}"#).await;
    assert!(!resp.ok, "an unknown provider is an error, suffix or not");
    assert_eq!(resp.error.as_deref(), Some("unknown provider: nope"));

    handle.abort();
}

// ── Test 3: :cache returns true after a warm get ─────────────────────────────

#[tokio::test]
async fn cache_returns_true_after_warm_get() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

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
    let handle = server.spawn().unwrap();

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

// ── Test 5: :age returns a JSON number ───────────────────────────────────────

#[tokio::test]
async fn age_returns_number() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    // Warm first.
    let warm = send_one(&sock, r#"{"op":"get","key":"hostname"}"#).await;
    assert!(warm.ok);

    // The cache stamps entries with `tokio::time::Instant`, so advancing the
    // paused clock ages the entry by exactly 5ms; the offset survives `resume`.
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_millis(5)).await;
    tokio::time::resume();

    let resp = send_one(&sock, r#"{"op":"get","key":"hostname:age"}"#).await;
    assert!(resp.ok, "age query should succeed");

    let data = resp.data.expect("missing data");
    assert!(data.is_number(), "age must be a JSON number, got {data:?}");
    assert!(data.as_u64().expect("age must be u64") > 0);

    handle.abort();
}

// ── Test 6: unknown suffix is passed through as a field name ─────────────────

#[tokio::test]
async fn unknown_suffix_is_passed_through_as_field() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

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
