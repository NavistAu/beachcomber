use beachcomber::cache::Cache;
use beachcomber::protocol::Response;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{ProviderResult, Value};
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

/// force=true: evicts the stale cache entry and re-executes the provider,
/// returning age_ms=0 (fresh inline execution) rather than the old age.
#[tokio::test]
async fn get_force_evicts_and_re_executes() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    // Pre-seed the cache with a value for the hostname provider so a normal
    // get would return a hit with age > 0.
    let mut result = ProviderResult::new();
    result.insert("name", Value::String("stale-host".to_string()));
    result.insert("short", Value::String("stale".to_string()));
    cache.put("hostname", None, result);

    // Confirm the cache has the stale entry.
    assert!(
        cache.get("hostname", None).is_some(),
        "cache should have the seeded entry"
    );

    let server = Server::new(sock.clone(), Arc::clone(&cache), registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Send a force=true get for hostname.
    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op":"get","key":"hostname","force":true}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok, "force get should return ok");
    let data = response.data.expect("force get should return data");

    // The provider was re-executed inline: the result comes from the real
    // hostname provider, not the seeded "stale-host" value.
    let name = data["name"].as_str().expect("hostname.name should be a string");
    assert!(
        !name.is_empty(),
        "hostname.name should not be empty after force re-execution"
    );
    assert_ne!(
        name, "stale-host",
        "force should have evicted the seeded value and re-executed"
    );

    // age_ms should be 0 because it was a fresh inline execution.
    assert_eq!(
        response.age_ms,
        Some(0),
        "force re-execution should return age_ms=0"
    );

    handle.abort();
}

/// force=false (default): a cached entry is returned as a hit.
/// This exercises the control path that force=true deviates from.
#[tokio::test]
async fn get_without_force_returns_cached_entry() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    let mut result = ProviderResult::new();
    result.insert("name", Value::String("cached-host".to_string()));
    result.insert("short", Value::String("cached".to_string()));
    cache.put("hostname", None, result);

    let server = Server::new(sock.clone(), Arc::clone(&cache), registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op":"get","key":"hostname"}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok);
    let data = response.data.expect("should return cached data");
    assert_eq!(
        data["name"].as_str(),
        Some("cached-host"),
        "without force, cache hit should return the seeded value"
    );

    handle.abort();
}

/// wait field is accepted without error (no-op in this task; semantics land in T14).
#[tokio::test]
async fn get_with_wait_field_is_accepted() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    let server = Server::new(sock.clone(), Arc::clone(&cache), registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op":"get","key":"hostname","wait":true}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let response: Response = serde_json::from_str(&line).unwrap();
    // Must not error out — wait is wired through but currently a no-op.
    assert!(response.ok, "wait=true should not cause a protocol error");

    handle.abort();
}
