use beachcomber::cache::Cache;
use beachcomber::client::Client;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{ProviderResult, Value};
use beachcomber::server::Server;
use std::sync::Arc;
use tempfile::TempDir;

async fn setup_server() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(ProviderRegistry::with_defaults());

    // Pre-populate cache
    let mut hostname = ProviderResult::new();
    hostname.insert("name", Value::String("testhost.local".to_string()));
    hostname.insert("short", Value::String("testhost".to_string()));
    cache.put("hostname", None, hostname);

    let mut user = ProviderResult::new();
    user.insert("name", Value::String("testuser".to_string()));
    user.insert("uid", Value::Int(501));
    cache.put("user", None, user);

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    (tmp, sock)
}

#[tokio::test]
async fn client_get_full_provider() {
    let (_tmp, sock) = setup_server().await;
    let client = Client::new(sock);
    let response = client.get("hostname", None).await.unwrap();
    assert!(response.ok);
    let data = response.data.unwrap();
    assert_eq!(data["name"], "testhost.local");
    assert_eq!(data["short"], "testhost");
}

#[tokio::test]
async fn client_get_single_field() {
    let (_tmp, sock) = setup_server().await;
    let client = Client::new(sock);
    let response = client.get("hostname.short", None).await.unwrap();
    assert!(response.ok);
    assert_eq!(response.data.unwrap(), serde_json::json!("testhost"));
}

#[tokio::test]
async fn client_get_text_format() {
    let (_tmp, sock) = setup_server().await;
    let client = Client::new(sock);
    let text = client.get_text("hostname.name", None).await.unwrap();
    assert_eq!(text, "testhost.local");
}

#[tokio::test]
async fn client_get_unknown_provider() {
    let (_tmp, sock) = setup_server().await;
    let client = Client::new(sock);
    let response = client.get("nonexistent", None).await.unwrap();
    assert!(!response.ok);
    assert!(response.error.unwrap().contains("unknown provider"));
}

#[tokio::test]
async fn client_refresh() {
    let (_tmp, sock) = setup_server().await;
    let client = Client::new(sock);
    let response = client.refresh("hostname", None).await.unwrap();
    assert!(response.ok);
}

#[tokio::test]
async fn client_store_and_get() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let config = beachcomber::config::Config::load();
    let handle = beachcomber::daemon::start_in_process(sock.clone(), config);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = beachcomber::client::Client::new(sock.clone());

    let resp = client
        .store("testapp", serde_json::json!({"status": "ok"}), None, None)
        .await
        .unwrap();
    assert!(resp.ok);

    let resp = client.get("testapp.status", None).await.unwrap();
    assert!(resp.ok);
    assert_eq!(resp.data.unwrap(), "ok");

    handle.abort();
}
