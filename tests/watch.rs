use beachcomber::cache::Cache;
use beachcomber::protocol::Response;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{ProviderResult, Value};
use beachcomber::server::Server;
use beachcomber::watcher_registry::WatcherRegistry;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

fn setup() -> (
    TempDir,
    std::path::PathBuf,
    Arc<Cache>,
    Arc<ProviderRegistry>,
    Arc<WatcherRegistry>,
) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let watchers = Arc::new(WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(ProviderRegistry::with_defaults());
    (tmp, sock, cache, registry, watchers)
}

#[tokio::test]
async fn watch_receives_initial_value() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    let mut result = ProviderResult::new();
    result.insert("branch", Value::String("main".to_string()));
    result.insert("dirty", Value::Bool(false));
    cache.put("git", None, result);

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    writer
        .write_all(b"{\"op\":\"watch\",\"key\":\"git.branch\"}\n")
        .await
        .unwrap();

    let mut line = String::new();
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        reader.read_line(&mut line),
    )
    .await
    .expect("timed out waiting for initial watch value")
    .unwrap();
    assert!(result > 0, "Expected a response line");

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok, "Response should be ok: {:?}", response.error);
    assert_eq!(
        response.data.unwrap(),
        serde_json::json!("main"),
        "Initial value should be 'main'"
    );

    handle.abort();
}

#[tokio::test]
async fn watch_receives_updates() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    let mut result = ProviderResult::new();
    result.insert("branch", Value::String("main".to_string()));
    result.insert("dirty", Value::Bool(false));
    cache.put("git", None, result);

    let cache_writer = cache.clone();

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    writer
        .write_all(b"{\"op\":\"watch\",\"key\":\"git.branch\"}\n")
        .await
        .unwrap();

    // Read initial value
    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_millis(500),
        reader.read_line(&mut line),
    )
    .await
    .expect("timed out waiting for initial value")
    .unwrap();
    let response: Response = serde_json::from_str(&line).unwrap();
    assert_eq!(response.data.unwrap(), serde_json::json!("main"));

    // Update cache with new branch
    let mut updated = ProviderResult::new();
    updated.insert("branch", Value::String("feature/new-branch".to_string()));
    updated.insert("dirty", Value::Bool(false));
    cache_writer.put("git", None, updated);

    // Read the update
    line.clear();
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        reader.read_line(&mut line),
    )
    .await
    .expect("timed out waiting for update")
    .unwrap();
    assert!(result > 0, "Expected an update line");

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok);
    assert_eq!(
        response.data.unwrap(),
        serde_json::json!("feature/new-branch"),
        "Should receive updated branch name"
    );

    handle.abort();
}

#[tokio::test]
async fn watch_filters_unchanged_field() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    let mut result = ProviderResult::new();
    result.insert("branch", Value::String("main".to_string()));
    result.insert("dirty", Value::Bool(false));
    cache.put("git", None, result);

    let cache_writer = cache.clone();

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    writer
        .write_all(b"{\"op\":\"watch\",\"key\":\"git.branch\"}\n")
        .await
        .unwrap();

    // Read initial value
    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_millis(500),
        reader.read_line(&mut line),
    )
    .await
    .expect("timed out waiting for initial value")
    .unwrap();
    let response: Response = serde_json::from_str(&line).unwrap();
    assert_eq!(response.data.unwrap(), serde_json::json!("main"));

    // Update cache — change only dirty, branch stays "main"
    let mut updated = ProviderResult::new();
    updated.insert("branch", Value::String("main".to_string()));
    updated.insert("dirty", Value::Bool(true));
    cache_writer.put("git", None, updated);

    // Should NOT receive any update — branch didn't change
    line.clear();
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        reader.read_line(&mut line),
    )
    .await;
    assert!(
        result.is_err(),
        "Should timeout — no update expected when watched field is unchanged"
    );

    handle.abort();
}

#[tokio::test]
async fn watch_provider_level_gets_all_changes() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    let mut result = ProviderResult::new();
    result.insert("branch", Value::String("main".to_string()));
    result.insert("dirty", Value::Bool(false));
    cache.put("git", None, result);

    let cache_writer = cache.clone();

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // Watch "git" — no field, gets whole provider
    writer
        .write_all(b"{\"op\":\"watch\",\"key\":\"git\"}\n")
        .await
        .unwrap();

    // Read initial value
    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_millis(500),
        reader.read_line(&mut line),
    )
    .await
    .expect("timed out waiting for initial value")
    .unwrap();
    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok);
    let data = response.data.unwrap();
    assert_eq!(data["branch"], "main");

    // Update cache — change only dirty, branch stays "main"
    // Provider-level watch should still fire since the overall object changed
    let mut updated = ProviderResult::new();
    updated.insert("branch", Value::String("main".to_string()));
    updated.insert("dirty", Value::Bool(true));
    cache_writer.put("git", None, updated);

    // Should receive an update
    line.clear();
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        reader.read_line(&mut line),
    )
    .await
    .expect("timed out waiting for update")
    .unwrap();
    assert!(result > 0, "Expected an update line");

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok);
    let data = response.data.unwrap();
    assert_eq!(data["dirty"], true, "dirty should now be true");

    handle.abort();
}

#[tokio::test]
async fn watch_miss_then_update() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    // Do NOT pre-populate cache
    let cache_writer = cache.clone();

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    writer
        .write_all(b"{\"op\":\"watch\",\"key\":\"git.branch\"}\n")
        .await
        .unwrap();

    // Read initial value — should be a miss (data is None)
    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_millis(500),
        reader.read_line(&mut line),
    )
    .await
    .expect("timed out waiting for initial miss response")
    .unwrap();
    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok, "Miss should still be ok");
    assert!(
        response.data.is_none(),
        "Initial miss should have no data: {:?}",
        response.data
    );

    // Now populate the cache
    let mut result = ProviderResult::new();
    result.insert("branch", Value::String("main".to_string()));
    result.insert("dirty", Value::Bool(false));
    cache_writer.put("git", None, result);

    // Should receive the populated value
    line.clear();
    let bytes_read = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        reader.read_line(&mut line),
    )
    .await
    .expect("timed out waiting for update after cache population")
    .unwrap();
    assert!(bytes_read > 0, "Expected an update line");

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok);
    assert_eq!(
        response.data.unwrap(),
        serde_json::json!("main"),
        "Should receive the branch value after cache is populated"
    );

    handle.abort();
}
