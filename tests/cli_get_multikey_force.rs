// Verify that a multi-key `comb get` propagates force=true and wait=true to
// every fetch. Phase 1 Round 3 bug: session.get_formatted dropped both.

use beachcomber::cache::Cache;
use beachcomber::client::Client;
use beachcomber::provider::Value;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::server::Server;
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;

async fn setup_seeded() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(ProviderRegistry::with_defaults());

    // Fresh seed with 60s interval — force must evict this.
    let mut fields = HashMap::new();
    fields.insert(
        "name".to_string(),
        Value::String("seeded-value".to_string()),
    );
    fields.insert("short".to_string(), Value::String("seeded".to_string()));
    cache.put_source("hostname", None, "host", fields, Some(60));

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    server.spawn().unwrap();
    (tmp, sock)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_get_formatted_with_flags_exists_and_forwards_force() {
    // The method must exist on Session (Task 1) and force=true must evict
    // a non-stale seeded entry, returning the live provider value.
    let (_tmp, sock) = setup_seeded().await;
    let client = Client::new(sock);
    let mut session = client.connect().unwrap();

    let text = session
        .get_formatted_with_flags("hostname.name", None, "text", true, false)
        .unwrap();

    assert!(
        !text.contains("seeded-value"),
        "force=true must evict the fresh seed and re-execute; got {text:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_get_formatted_with_flags_forwards_wait() {
    // Calling get_formatted_with_flags with wait=true on a fresh entry must
    // not error. This locks in that wait is an accepted parameter; the
    // behavioral wait=true semantics (stale-re-execute) are covered in
    // tests/cli_get_wait_propagation.rs and tests/get_wait.rs.
    let (_tmp, sock) = setup_seeded().await;
    let client = Client::new(sock);
    let mut session = client.connect().unwrap();

    let text = session
        .get_formatted_with_flags("hostname.name", None, "text", false, true)
        .unwrap();

    // Fresh + wait=true + not stale => returns cached seed.
    assert!(
        text.contains("seeded-value"),
        "wait=true on a fresh entry must return the cached value; got {text:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn comb_get_multikey_forwards_force_to_every_fetch() {
    // `--force` on a multi-key get reaches each key's fetch: both seeded
    // hostname fields are evicted and re-executed, so neither seed is printed.
    let (tmp, sock) = setup_seeded().await;

    let stdout = tokio::task::spawn_blocking(move || {
        let out = assert_cmd::Command::cargo_bin("comb")
            .unwrap()
            .env("BEACHCOMBER_SOCKET", &sock)
            .env("RUST_LOG", "error")
            .env("HOME", tmp.path())
            .env("XDG_CONFIG_HOME", tmp.path().join("cfg"))
            .args(["get", "--force", "hostname.name", "hostname.short"])
            .assert()
            .success();
        String::from_utf8(out.get_output().stdout.clone()).unwrap()
    })
    .await
    .unwrap();

    assert!(
        !stdout.contains("seeded"),
        "--force must evict every seeded field and re-execute; got {stdout:?}"
    );
}
