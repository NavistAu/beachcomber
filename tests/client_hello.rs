// Verifies the Rust client's hello() API.

use beachcomber::cache::Cache;
use beachcomber::client::Client;
use beachcomber::protocol::PROTOCOL_VERSION;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::server::Server;
use std::sync::Arc;
use tempfile::TempDir;

async fn setup() -> (TempDir, std::path::PathBuf) {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_hello_returns_protocol_version() {
    let (_tmp, sock) = setup().await;
    let client = Client::new(sock);
    let response = client.hello().expect("hello should succeed");
    assert!(response.ok, "hello response ok=true, got {response:?}");
    let data = response.data.expect("hello response must have data");
    assert_eq!(
        data["protocol_version"].as_str(),
        Some(PROTOCOL_VERSION),
        "client.hello() must surface the server's protocol_version"
    );
    assert!(
        data["daemon_version"].is_string(),
        "client.hello() must surface daemon_version as a string"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_hello_works_on_persistent_connection() {
    let (_tmp, sock) = setup().await;
    let client = Client::new(sock);
    let mut session = client.connect().expect("connect");
    let response = session.hello().expect("session.hello should succeed");
    assert!(response.ok);
    assert!(response.data.is_some());
}
