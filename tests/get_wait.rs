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

/// wait=true + fresh entry (interval=60s) -> returns the cached value without re-executing.
/// The returned data must equal the seeded value (not a live provider result).
#[tokio::test]
async fn wait_fresh_entry_is_served_from_cache() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    // Put a fresh entry with a 60-second interval — it cannot be stale yet.
    let mut result = ProviderResult::new();
    result.insert("name", Value::String("cached-host".to_string()));
    result.insert("short", Value::String("cached".to_string()));
    cache.put_with_interval("hostname", None, result, Some(60));

    // Sanity-check the entry is not stale.
    let entry = cache.get("hostname", None).expect("entry must be present");
    assert!(!entry.is_stale(), "entry with 60s interval must not be stale immediately");

    let server = Server::new(sock.clone(), Arc::clone(&cache), registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    stream
        .write_all(b"{\"op\":\"get\",\"key\":\"hostname\",\"wait\":true}\n")
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok, "wait on fresh entry should succeed");
    let data = response.data.expect("wait on fresh entry should return data");

    // Fresh path: must return the seeded value, not the live provider output.
    assert_eq!(
        data["name"].as_str(),
        Some("cached-host"),
        "wait on fresh entry must return the cached value"
    );

    // age_ms should be > 0 because it was served from cache (not re-executed inline).
    assert!(
        response.age_ms.unwrap_or(0) > 0,
        "wait on fresh entry should report age > 0 (cache hit, not re-executed)"
    );

    handle.abort();
}

/// wait=true + stale entry (interval=0s) -> evicts the entry and re-executes inline,
/// returning age_ms=0 and the live provider value (not the seeded stale value).
#[tokio::test]
async fn wait_stale_entry_evicts_and_re_executes() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    // interval=0 means stale after 0 seconds (elapsed > 0 is true immediately after 1s).
    let mut result = ProviderResult::new();
    result.insert("name", Value::String("stale-host".to_string()));
    result.insert("short", Value::String("stale".to_string()));
    cache.put_with_interval("hostname", None, result, Some(0));

    // Wait 1 second so elapsed().as_secs() > 0 (which makes is_stale() return true).
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    // Confirm it is stale before we send the request.
    let entry = cache.get("hostname", None).expect("entry must be present");
    assert!(entry.is_stale(), "entry with interval=0 must be stale after 1s");

    let server = Server::new(sock.clone(), Arc::clone(&cache), registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    stream
        .write_all(b"{\"op\":\"get\",\"key\":\"hostname\",\"wait\":true}\n")
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok, "wait on stale entry should succeed after re-execution");
    let data = response.data.expect("wait on stale entry should return data");

    // The provider was re-executed: the result must not be the seeded stale value.
    let name = data["name"].as_str().expect("hostname.name must be a string");
    assert!(
        !name.is_empty(),
        "hostname.name must not be empty after re-execution"
    );
    assert_ne!(
        name, "stale-host",
        "wait on stale entry must evict and re-execute, not return the old value"
    );

    // age_ms=0 indicates it was a fresh inline execution (not served from cache).
    assert_eq!(
        response.age_ms,
        Some(0),
        "wait on stale entry must return age_ms=0 (inline re-execution)"
    );

    handle.abort();
}

/// wait=true + virtual provider -> returns the cached value regardless of staleness.
/// Virtual providers have no source to re-execute, so the entry is served as-is.
#[tokio::test]
async fn wait_virtual_provider_returns_cached_ignoring_stale() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    // Register a virtual provider and seed an entry that will appear stale.
    registry.register_virtual("mystore");

    let mut result = ProviderResult::new();
    result.insert("val", Value::String("virtual-value".to_string()));
    // interval=0 will make it stale after 1 second.
    cache.put_with_interval("mystore", None, result, Some(0));

    // Wait so the entry becomes stale.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    let entry = cache.get("mystore", None).expect("entry must be present");
    assert!(entry.is_stale(), "virtual entry with interval=0 must be stale after 1s");

    let server = Server::new(sock.clone(), Arc::clone(&cache), registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    stream
        .write_all(b"{\"op\":\"get\",\"key\":\"mystore\",\"wait\":true}\n")
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let response: Response = serde_json::from_str(&line).unwrap();
    // Virtual provider: wait must not error and must return the cached value.
    assert!(
        response.ok,
        "wait on stale virtual provider must succeed (return cached, not error)"
    );
    let data = response.data.expect("wait on virtual provider must return data");
    assert_eq!(
        data["val"].as_str(),
        Some("virtual-value"),
        "wait on virtual provider must return the cached value as-is"
    );

    handle.abort();
}

/// force=true wins over wait=true: the entry is always evicted and re-executed,
/// regardless of whether wait is also set.
#[tokio::test]
async fn force_wins_over_wait() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    // Seed a fresh (non-stale) entry. With wait only, this would be a cache hit.
    // With force, it must be evicted and re-executed regardless.
    let mut result = ProviderResult::new();
    result.insert("name", Value::String("seeded-host".to_string()));
    result.insert("short", Value::String("seeded".to_string()));
    cache.put_with_interval("hostname", None, result, Some(60));

    let server = Server::new(sock.clone(), Arc::clone(&cache), registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    stream
        .write_all(b"{\"op\":\"get\",\"key\":\"hostname\",\"force\":true,\"wait\":true}\n")
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok, "force+wait should succeed");
    let data = response.data.expect("force+wait should return data");

    let name = data["name"].as_str().expect("hostname.name must be a string");
    assert!(
        !name.is_empty(),
        "hostname.name must not be empty after force re-execution"
    );
    // force evicted the entry, so the live provider ran — not the seeded value.
    assert_ne!(
        name, "seeded-host",
        "force must win over wait: seeded value must be evicted and live value returned"
    );
    assert_eq!(
        response.age_ms,
        Some(0),
        "force re-execution must return age_ms=0"
    );

    handle.abort();
}
