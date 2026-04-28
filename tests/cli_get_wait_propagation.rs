// Locks in wait=true semantics at the Client API layer that run_get delegates
// to: Client::get_formatted_with_flags (single-key server-side text path) and
// ClientSession::get_with_flags (multi-key client-side loop).
//
// run_get itself lives in the binary crate and cannot be called from an
// integration test; its flag-threading is guarded by the compiler + clippy
// (the previously-unused `_wait` would re-trigger a warning if it came back).
//
// Phase 1 Round 3 bug: run_get captured `wait` then discarded it via `_wait`,
// so every CLI --wait invocation silently behaved as wait=false.

use beachcomber::cache::Cache;
use beachcomber::client::Client;
use beachcomber::provider::Value;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::server::Server;
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;

async fn setup_stale() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(ProviderRegistry::with_defaults());

    // Seed hostname with a stale entry. interval=0 => stale after 0 elapsed secs.
    let mut fields = HashMap::new();
    fields.insert("name".to_string(), Value::String("stale-seed".to_string()));
    fields.insert("short".to_string(), Value::String("stale".to_string()));
    cache.put_source("hostname", None, "main", fields, Some(0));

    // Advance mock clock by 2s so elapsed().as_secs() > 0 — avoids a real 1.1s wall wait.
    // Resume before starting the server so the socket-bind sleep runs on real time.
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    tokio::time::resume();

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (tmp, sock)
}

#[tokio::test]
async fn single_key_text_wait_true_re_executes_stale_entry() {
    // run_get's single-key server-side path (format=text, keys.len()==1) must
    // forward wait=true. If it does, the stale seeded value is evicted and the
    // live provider runs inline — the returned text must not contain the seed.
    let (_tmp, sock) = setup_stale().await;
    let client = Client::new(sock);

    let text = client
        .get_formatted_with_flags("hostname.name", None, "text", false, true)
        .await
        .unwrap();

    assert!(
        !text.contains("stale-seed"),
        "wait=true on a stale entry must trigger re-execution; got {text:?}"
    );
}

#[tokio::test]
async fn multi_key_client_side_wait_true_re_executes_stale_entry() {
    // run_get's multi-key client-side path (e.g. format=json) calls
    // session.get_with_flags per key. It must forward wait=true.
    let (_tmp, sock) = setup_stale().await;
    let client = Client::new(sock);
    let mut session = client.connect().await.unwrap();

    let response = session
        .get_with_flags("hostname.name", None, false, true)
        .await
        .unwrap();

    assert!(response.ok);
    let data = response.data.expect("data present");
    let name = data.as_str().expect("name field is a string");
    assert_ne!(
        name, "stale-seed",
        "wait=true on a stale entry must trigger re-execution; got {name:?}"
    );
    assert_eq!(
        response.age_ms,
        Some(0),
        "wait re-execution must return age_ms=0"
    );
}
