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
) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    (tmp, sock, cache, registry)
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

#[tokio::test]
async fn store_and_get_roundtrip() {
    let (_tmp, sock, cache, registry) = setup();
    let server = Server::new(sock.clone(), cache, registry, None);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    // Store data
    let store_req = r#"{"op":"store","key":"myapp","data":{"status":"healthy","version":"1.2.3"}}"#;
    let store_resp = send_recv(&mut stream, store_req).await;
    assert!(
        store_resp.ok,
        "store should succeed: {:?}",
        store_resp.error
    );

    // Get all fields
    let get_req = r#"{"op":"get","key":"myapp"}"#;
    let get_resp = send_recv(&mut stream, get_req).await;
    assert!(get_resp.ok, "get should succeed: {:?}", get_resp.error);
    let data = get_resp.data.unwrap();
    assert_eq!(data["status"], "healthy");
    assert_eq!(data["version"], "1.2.3");

    // Get single field
    let get_field_req = r#"{"op":"get","key":"myapp.status"}"#;
    let get_field_resp = send_recv(&mut stream, get_field_req).await;
    assert!(get_field_resp.ok);
    assert_eq!(get_field_resp.data.unwrap(), serde_json::json!("healthy"));

    handle.abort();
}

#[tokio::test]
async fn store_rejects_builtin_name() {
    let (_tmp, sock, cache, registry) = setup();
    let server = Server::new(sock.clone(), cache, registry, None);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    let req = r#"{"op":"store","key":"git","data":{"branch":"main"}}"#;
    let resp = send_recv(&mut stream, req).await;
    assert!(!resp.ok, "store under builtin name should fail");
    let err = resp.error.unwrap();
    assert!(
        err.contains("builtin") || err.contains("script"),
        "error should mention builtin or script, got: {err}"
    );

    handle.abort();
}

#[tokio::test]
async fn store_replaces_previous_data() {
    let (_tmp, sock, cache, registry) = setup();
    let server = Server::new(sock.clone(), cache, registry, None);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    // Store v1
    let v1 = r#"{"op":"store","key":"myapp","data":{"status":"starting","old_field":"yes"}}"#;
    let r = send_recv(&mut stream, v1).await;
    assert!(r.ok);

    // Store v2 with different fields
    let v2 = r#"{"op":"store","key":"myapp","data":{"status":"ready","version":"2.0"}}"#;
    let r = send_recv(&mut stream, v2).await;
    assert!(r.ok);

    // Get should return v2 data only
    let get = r#"{"op":"get","key":"myapp"}"#;
    let resp = send_recv(&mut stream, get).await;
    assert!(resp.ok);
    let data = resp.data.unwrap();
    assert_eq!(data["status"], "ready");
    assert_eq!(data["version"], "2.0");
    // old_field should be absent since we replaced the whole entry
    assert!(data.get("old_field").is_none() || data["old_field"].is_null());

    handle.abort();
}

#[tokio::test]
async fn store_with_ttl_shows_staleness() {
    let (_tmp, sock, cache, registry) = setup();
    let server = Server::new(sock.clone(), cache, registry, None);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    // Store with TTL of 1 second
    let req = r#"{"op":"store","key":"myapp","data":{"status":"ok"},"ttl":"1s"}"#;
    let r = send_recv(&mut stream, req).await;
    assert!(r.ok);

    // Immediately get — should not be stale
    let get = r#"{"op":"get","key":"myapp"}"#;
    let resp = send_recv(&mut stream, get).await;
    assert!(resp.ok);
    assert_eq!(
        resp.stale,
        Some(false),
        "should not be stale immediately after store"
    );

    // Wait 1.1s beyond the 1s TTL; as_secs() truncates so we need >2s total elapsed
    // for elapsed.as_secs() > 1 to be true.
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;

    let resp2 = send_recv(&mut stream, get).await;
    assert!(resp2.ok);
    assert_eq!(resp2.stale, Some(true), "should be stale after TTL expires");

    handle.abort();
}

#[tokio::test]
async fn store_appears_in_list() {
    let (_tmp, sock, cache, registry) = setup();
    let server = Server::new(sock.clone(), cache, registry, None);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    // Store something
    let req = r#"{"op":"store","key":"myapp","data":{"status":"ok"}}"#;
    let r = send_recv(&mut stream, req).await;
    assert!(r.ok);

    // List should include "myapp"
    let list_req = r#"{"op":"list"}"#;
    let list_resp = send_recv(&mut stream, list_req).await;
    assert!(list_resp.ok);
    let providers = list_resp.data.unwrap();
    let names: Vec<&str> = providers
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"myapp"),
        "myapp should appear in list, got: {names:?}"
    );

    handle.abort();
}

#[tokio::test]
async fn poke_virtual_provider_is_noop() {
    let (_tmp, sock, cache, registry) = setup();
    let server = Server::new(sock.clone(), cache, registry, None);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    send_recv(
        &mut stream,
        r#"{"op":"store","key":"myapp","data":{"v":"1"}}"#,
    )
    .await;

    let resp = send_recv(&mut stream, r#"{"op":"poke","key":"myapp"}"#).await;
    assert!(resp.ok);

    let resp = send_recv(&mut stream, r#"{"op":"get","key":"myapp.v"}"#).await;
    assert_eq!(resp.data.unwrap(), "1");

    handle.abort();
}

#[tokio::test]
async fn store_with_path_scope() {
    let (_tmp, sock, cache, registry) = setup();
    let server = Server::new(sock.clone(), cache, registry, None);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    send_recv(
        &mut stream,
        r#"{"op":"store","key":"myapp","data":{"v":"proj-a"},"path":"/tmp/proj-a"}"#,
    )
    .await;
    send_recv(
        &mut stream,
        r#"{"op":"store","key":"myapp","data":{"v":"proj-b"},"path":"/tmp/proj-b"}"#,
    )
    .await;

    let resp = send_recv(
        &mut stream,
        r#"{"op":"get","key":"myapp.v","path":"/tmp/proj-a"}"#,
    )
    .await;
    assert_eq!(resp.data.unwrap(), "proj-a");

    let resp = send_recv(
        &mut stream,
        r#"{"op":"get","key":"myapp.v","path":"/tmp/proj-b"}"#,
    )
    .await;
    assert_eq!(resp.data.unwrap(), "proj-b");

    handle.abort();
}
