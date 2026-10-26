use beachcomber::cache::Cache;
use beachcomber::client::Client;
use beachcomber::config::Config;
use beachcomber::scheduler::Scheduler;
use beachcomber::server::Server;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

use crate::common::fetch::fetch;
use crate::common::poll::poll_until_async;

async fn setup_with_scheduler() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(crate::common::registry::with_shipped_defaults());
    let config = Config::default();

    // No wait needed after spawning: SchedulerHandle::send goes through an mpsc
    // channel that buffers until run() starts receiving.
    let (handle, mut scheduler) =
        Scheduler::new(cache.clone(), registry.clone(), config, watchers.clone());
    scheduler.use_polling_watcher(Duration::from_millis(50));
    tokio::spawn(async move { scheduler.run().await });

    // Server::spawn binds before returning, so the socket is connectable the
    // moment this line completes — nothing to sleep on.
    let server = Server::new(sock.clone(), cache, registry, Some(handle), watchers);
    server.spawn().expect("server should bind");

    (tmp, sock)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_goes_through_scheduler() {
    let (_tmp, sock) = setup_with_scheduler().await;
    let client = Client::new(sock.clone());

    let response = client.refresh("hostname", None).unwrap();
    assert!(response.ok);

    let data = poll_until_async(
        "client get hostname",
        "ok with data",
        Duration::from_secs(5),
        Duration::from_millis(10),
        async || {
            fetch(&sock, "hostname", None)
                .unwrap()
                .and_then(|got| got.data)
        },
        |data| data.is_some(),
    )
    .await;
    assert!(data.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn once_providers_available_via_scheduler() {
    let (_tmp, sock) = setup_with_scheduler().await;
    let response = fetch(&sock, "hostname.name", None).unwrap().unwrap();
    assert!(response.data.is_some());
}
