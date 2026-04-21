use beachcomber::cache::Cache;
use beachcomber::client::Client;
use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::scheduler::Scheduler;
use beachcomber::server::Server;
use std::sync::Arc;
use tempfile::TempDir;

async fn setup_with_scheduler() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let config = Config::default();

    let (handle, scheduler) = Scheduler::new(cache.clone(), registry.clone(), config);
    tokio::spawn(async move { scheduler.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let server = Server::new(sock.clone(), cache, registry, Some(handle), watchers);
    tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    (tmp, sock)
}

#[tokio::test]
async fn refresh_goes_through_scheduler() {
    let (_tmp, sock) = setup_with_scheduler().await;
    let client = Client::new(sock);

    let response = client.refresh("hostname", None).await.unwrap();
    assert!(response.ok);

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let response = client.get("hostname", None).await.unwrap();
    assert!(response.ok);
    assert!(response.data.is_some());
}

#[tokio::test]
async fn once_providers_available_via_scheduler() {
    let (_tmp, sock) = setup_with_scheduler().await;
    let client = Client::new(sock);

    let response = client.get("hostname.name", None).await.unwrap();
    assert!(response.ok);
    assert!(response.data.is_some());
}
