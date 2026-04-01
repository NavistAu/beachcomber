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
) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    (tmp, sock, cache, registry)
}

#[tokio::test]
async fn server_accepts_connection() {
    let (_tmp, sock, cache, registry) = setup();
    let server = Server::new(sock.clone(), cache, registry, None);

    let handle = tokio::spawn(async move { server.run().await });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&sock).await;
    assert!(stream.is_ok(), "Should connect to server socket");

    handle.abort();
}

#[tokio::test]
async fn server_handles_get_global_provider() {
    let (_tmp, sock, cache, registry) = setup();

    let mut result = ProviderResult::new();
    result.insert("name", Value::String("testhost.local".to_string()));
    result.insert("short", Value::String("testhost".to_string()));
    cache.put("hostname", None, result);

    let server = Server::new(sock.clone(), cache, registry, None);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op": "get", "key": "hostname"}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok, "Response should be ok");
    let data = response.data.unwrap();
    assert_eq!(data["name"], "testhost.local");
    assert_eq!(data["short"], "testhost");

    handle.abort();
}

#[tokio::test]
async fn server_handles_get_single_field() {
    let (_tmp, sock, cache, registry) = setup();

    let mut result = ProviderResult::new();
    result.insert("name", Value::String("testhost.local".to_string()));
    result.insert("short", Value::String("testhost".to_string()));
    cache.put("hostname", None, result);

    let server = Server::new(sock.clone(), cache, registry, None);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op": "get", "key": "hostname.short"}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok);
    assert_eq!(response.data.unwrap(), serde_json::json!("testhost"));

    handle.abort();
}

#[tokio::test]
async fn server_handles_get_text_format() {
    let (_tmp, sock, cache, registry) = setup();

    let mut result = ProviderResult::new();
    result.insert("name", Value::String("testhost.local".to_string()));
    cache.put("hostname", None, result);

    let server = Server::new(sock.clone(), cache, registry, None);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op": "get", "key": "hostname.name", "format": "text"}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    // Text format for single field: raw value followed by newline
    assert_eq!(line.trim(), "testhost.local");

    handle.abort();
}

#[tokio::test]
async fn server_handles_cache_miss() {
    let (_tmp, sock, cache, registry) = setup();

    let server = Server::new(sock.clone(), cache, registry, None);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op": "get", "key": "hostname"}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok, "Cache miss should still be ok");
    assert!(response.data.is_none(), "Cache miss should have null data");

    handle.abort();
}

#[tokio::test]
async fn server_handles_unknown_provider() {
    let (_tmp, sock, cache, registry) = setup();

    let server = Server::new(sock.clone(), cache, registry, None);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op": "get", "key": "nonexistent"}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(!response.ok, "Unknown provider should return error");
    assert!(response.error.unwrap().contains("unknown provider"));

    handle.abort();
}

#[tokio::test]
async fn server_handles_poke() {
    let (_tmp, sock, cache, registry) = setup();

    let server = Server::new(sock.clone(), cache, registry, None);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op": "poke", "key": "hostname"}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok, "Poke should return ok");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    handle.abort();
}
