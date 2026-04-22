// Verify that the multi-key server-side text/sh path propagates force=true
// and wait=true. Phase 1 Round 3 bug: session.get_formatted dropped both.

use beachcomber::cache::Cache;
use beachcomber::client::Client;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{ProviderResult, Value};
use beachcomber::server::Server;
use std::sync::Arc;
use tempfile::TempDir;

async fn setup_seeded() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(ProviderRegistry::with_defaults());

    // Fresh seed with 60s interval — force must evict this.
    let mut result = ProviderResult::new();
    result.insert("name", Value::String("seeded-value".to_string()));
    result.insert("short", Value::String("seeded".to_string()));
    cache.put_with_interval("hostname", None, result, Some(60));

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (tmp, sock)
}

#[tokio::test]
async fn session_get_formatted_with_flags_exists_and_forwards_force() {
    // The method must exist on Session (Task 1) and force=true must evict
    // a non-stale seeded entry, returning the live provider value.
    let (_tmp, sock) = setup_seeded().await;
    let client = Client::new(sock);
    let mut session = client.connect().await.unwrap();

    let text = session
        .get_formatted_with_flags("hostname.name", None, "text", true, false)
        .await
        .unwrap();

    assert!(
        !text.contains("seeded-value"),
        "force=true must evict the fresh seed and re-execute; got {text:?}"
    );
}

#[tokio::test]
async fn session_get_formatted_with_flags_forwards_wait() {
    // wait must also be forwarded. A fresh entry with wait=true must be
    // served from cache (no-op for wait when not stale). This test locks
    // in that the parameter reaches the wire without error.
    let (_tmp, sock) = setup_seeded().await;
    let client = Client::new(sock);
    let mut session = client.connect().await.unwrap();

    let text = session
        .get_formatted_with_flags("hostname.name", None, "text", false, true)
        .await
        .unwrap();

    // Fresh + wait=true + not stale => returns cached seed.
    assert!(
        text.contains("seeded-value"),
        "wait=true on a fresh entry must return the cached value; got {text:?}"
    );
}
