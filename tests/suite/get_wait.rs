use beachcomber::cache::Schedule;
use beachcomber::cache::{Cache, PUT_SOURCE};
use beachcomber::protocol::Response;
use beachcomber::provider::Value;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::server::Server;
use std::collections::HashMap;
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
    let registry = Arc::new(crate::common::registry::with_shipped_defaults());
    (tmp, sock, cache, registry, watchers)
}

/// wait=true + fresh entry (interval=60s) -> returns the cached value without re-executing.
/// The returned data must equal the seeded value (not a live provider result).
#[tokio::test]
async fn wait_fresh_entry_is_served_from_cache() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    // Put a fresh entry with a 60-second interval — it cannot be stale yet.
    let mut fields = HashMap::new();
    fields.insert("name".to_string(), Value::String("cached-host".to_string()));
    fields.insert("short".to_string(), Value::String("cached".to_string()));
    cache.put_source("hostname", None, "host", fields, Schedule::poll(60, 1));

    // Age the record by ten seconds on the mock clock — well inside its 60 s
    // interval, so it stays fresh, and far enough that the `age_ms` assertion
    // below cannot depend on how long the put→get gap took in wall time.
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(10)).await;
    tokio::time::resume();

    // Sanity-check the entry is not stale.
    let entry = cache
        .get_entry("hostname", None)
        .expect("entry must be present");
    assert!(
        !entry.is_stale(),
        "entry with 60s interval must not be stale after 10 s"
    );

    let server = Server::new(sock.clone(), Arc::clone(&cache), registry, None, watchers);
    let handle = server.spawn().unwrap();

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
    let data = response
        .data
        .expect("wait on fresh entry should return data");

    // Fresh path: must return the seeded value, not the live provider output.
    assert_eq!(
        data["name"].as_str(),
        Some("cached-host"),
        "wait on fresh entry must return the cached value"
    );

    // Served from cache, not re-executed: the record carries the ten seconds
    // the clock advanced. A re-execution would have written a fresh record.
    assert!(
        response.age_ms.unwrap_or(0) >= 10_000,
        "wait on fresh entry should report the seeded record's age (cache hit, not re-executed), got {:?}",
        response.age_ms
    );

    handle.abort();
}

/// wait=true + stale entry (interval=0s) -> evicts the entry and re-executes inline,
/// returning age_ms=0 and the live provider value (not the seeded stale value).
#[tokio::test]
async fn wait_stale_entry_evicts_and_re_executes() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    // interval=0 means stale after 0 seconds (elapsed > 0 is true immediately after 1s).
    let mut fields = HashMap::new();
    fields.insert("name".to_string(), Value::String("stale-host".to_string()));
    fields.insert("short".to_string(), Value::String("stale".to_string()));
    cache.put_source("hostname", None, "host", fields, Schedule::poll(0, 1));

    // Advance mock clock by 2s so elapsed().as_secs() > 0 — avoids a real 1.1s wall wait.
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    tokio::time::resume();

    // Confirm it is stale before we send the request.
    let entry = cache
        .get_entry("hostname", None)
        .expect("entry must be present");
    assert!(
        entry.is_stale(),
        "entry with interval=0 must be stale after clock advance"
    );

    let server = Server::new(sock.clone(), Arc::clone(&cache), registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let asked = std::time::Instant::now();
    stream
        .write_all(b"{\"op\":\"get\",\"key\":\"hostname\",\"wait\":true}\n")
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let since_asked = asked.elapsed().as_millis();

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(
        response.ok,
        "wait on stale entry should succeed after re-execution"
    );
    let data = response
        .data
        .expect("wait on stale entry should return data");

    // The provider was re-executed: the result must not be the seeded stale value.
    let name = data["name"]
        .as_str()
        .expect("hostname.name must be a string");
    assert!(
        !name.is_empty(),
        "hostname.name must not be empty after re-execution"
    );
    assert_ne!(
        name, "stale-host",
        "wait on stale entry must evict and re-execute, not return the old value"
    );

    // The answer's age is no older than the request: a fresh inline execution,
    // not the 2s-old record served from cache.
    let age = response.age_ms.expect("an age");
    assert!(
        age <= since_asked,
        "wait on stale entry must return a record run for it: {age}ms old, asked {since_asked}ms ago"
    );

    handle.abort();
}

/// wait=true + static namespace -> returns the cached value regardless of staleness.
/// Static namespaces have no source to re-execute, so the entry is served as-is.
#[tokio::test]
async fn wait_static_namespace_returns_cached_ignoring_stale() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    // Register a static namespace and seed an entry that will appear stale.
    registry.register_static("mystore");

    let mut fields = HashMap::new();
    fields.insert("val".to_string(), Value::String("static-value".to_string()));
    // interval=0 will make it stale after 1 second.
    cache.put_source("mystore", None, PUT_SOURCE, fields, Schedule::poll(0, 1));

    // Advance mock clock by 2s so elapsed().as_secs() > 0 — avoids a real 1.1s wall wait.
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    tokio::time::resume();

    let entry = cache
        .get_entry("mystore", None)
        .expect("entry must be present");
    assert!(
        entry.is_stale(),
        "static entry with interval=0 must be stale after clock advance"
    );

    let server = Server::new(sock.clone(), Arc::clone(&cache), registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    stream
        .write_all(b"{\"op\":\"get\",\"key\":\"mystore\",\"wait\":true}\n")
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let response: Response = serde_json::from_str(&line).unwrap();
    // Static namespace: wait must not error and must return the cached value.
    assert!(
        response.ok,
        "wait on stale static namespace must succeed (return cached, not error)"
    );
    let data = response
        .data
        .expect("wait on static namespace must return data");
    assert_eq!(
        data["val"].as_str(),
        Some("static-value"),
        "wait on static namespace must return the cached value as-is"
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
    let mut fields = HashMap::new();
    fields.insert("name".to_string(), Value::String("seeded-host".to_string()));
    fields.insert("short".to_string(), Value::String("seeded".to_string()));
    cache.put_source("hostname", None, "host", fields, Schedule::poll(60, 1));

    let server = Server::new(sock.clone(), Arc::clone(&cache), registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let asked = std::time::Instant::now();
    stream
        .write_all(b"{\"op\":\"get\",\"key\":\"hostname\",\"force\":true,\"wait\":true}\n")
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let since_asked = asked.elapsed().as_millis();

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok, "force+wait should succeed");
    let data = response.data.expect("force+wait should return data");

    let name = data["name"]
        .as_str()
        .expect("hostname.name must be a string");
    assert!(
        !name.is_empty(),
        "hostname.name must not be empty after force re-execution"
    );
    // force evicted the entry, so the live provider ran — not the seeded value.
    assert_ne!(
        name, "seeded-host",
        "force must win over wait: seeded value must be evicted and live value returned"
    );
    let age = response.age_ms.expect("an age");
    assert!(
        age <= since_asked,
        "force re-execution must return a record run for it: {age}ms old, asked {since_asked}ms ago"
    );

    handle.abort();
}
