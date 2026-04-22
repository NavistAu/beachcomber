// Integration tests for the Hello op (Phase 2 of interface-architecture).
//
// Hello is the first op a client should send on a new connection. It returns
// { protocol_version, daemon_version } so the client can verify compatibility
// before proceeding.

use beachcomber::cache::Cache;
use beachcomber::protocol::PROTOCOL_VERSION;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::server::Server;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

async fn spawn_server() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (tmp, sock)
}

#[tokio::test]
async fn hello_returns_protocol_and_daemon_version() {
    let (_tmp, sock) = spawn_server().await;

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    writer.write_all(b"{\"op\":\"hello\"}\n").await.unwrap();

    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(parsed["ok"], serde_json::json!(true));
    let data = &parsed["data"];
    assert_eq!(
        data["protocol_version"].as_str(),
        Some(PROTOCOL_VERSION),
        "Hello response must include the current PROTOCOL_VERSION"
    );
    let daemon_version = data["daemon_version"].as_str();
    assert!(
        daemon_version.is_some() && !daemon_version.unwrap().is_empty(),
        "Hello response must include a non-empty daemon_version string"
    );
}

#[tokio::test]
async fn hello_does_not_require_prior_context() {
    // Hello must work as the very first op on a fresh connection, before
    // any Context or other setup op.
    let (_tmp, sock) = spawn_server().await;
    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    writer.write_all(b"{\"op\":\"hello\"}\n").await.unwrap();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(parsed["ok"], serde_json::json!(true));
}

#[tokio::test]
async fn hello_can_be_followed_by_other_ops_on_same_connection() {
    // After Hello, the connection remains usable for further ops.
    let (_tmp, sock) = spawn_server().await;
    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();

    writer.write_all(b"{\"op\":\"hello\"}\n").await.unwrap();
    writer
        .write_all(b"{\"op\":\"introspect\",\"subject\":\"daemon\"}\n")
        .await
        .unwrap();

    let mut reader = BufReader::new(reader);
    let mut line1 = String::new();
    reader.read_line(&mut line1).await.unwrap();
    let mut line2 = String::new();
    reader.read_line(&mut line2).await.unwrap();

    let p1: serde_json::Value = serde_json::from_str(line1.trim()).unwrap();
    let p2: serde_json::Value = serde_json::from_str(line2.trim()).unwrap();
    assert_eq!(p1["ok"], serde_json::json!(true), "hello failed: {p1:?}");
    assert_eq!(
        p2["ok"],
        serde_json::json!(true),
        "introspect failed: {p2:?}"
    );
    assert!(p1["data"]["protocol_version"].is_string());
    assert!(p2["data"]["pid"].is_number());
}

#[test]
fn protocol_version_follows_semver_shape() {
    // Protocol version is "MAJOR.MINOR" — no patch component, no pre-release.
    let parts: Vec<&str> = PROTOCOL_VERSION.split('.').collect();
    assert_eq!(
        parts.len(),
        2,
        "PROTOCOL_VERSION must be MAJOR.MINOR (got {PROTOCOL_VERSION:?})"
    );
    for part in parts {
        assert!(
            part.parse::<u32>().is_ok(),
            "PROTOCOL_VERSION parts must be non-negative integers (got {part:?})"
        );
    }
}
