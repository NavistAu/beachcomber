// Verify that the multi-key server-side text/sh path propagates force=true
// and wait=true. Phase 1 Round 3 bug: session.get_formatted dropped both.

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
    cache.put_source("hostname", None, "main", fields, Some(60));

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
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

#[test]
fn run_get_multikey_server_side_uses_get_formatted_with_flags() {
    // The multi-key server-side loop must call get_formatted_with_flags,
    // not the flag-less get_formatted. This guards against regression to
    // the Round 3 bug where force and wait were silently dropped.
    // run_get now lives in src/cli/commands/get.rs (moved in Task 2.2).
    // Whitespace-normalized so rustfmt's line-wrapping of this call (it no
    // longer shares a line with a trailing `.await`, and rustfmt adds a
    // trailing comma once it wraps) doesn't break the check.
    let src = std::fs::read_to_string("src/cli/commands/get.rs").unwrap();
    let normalized: String = src.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        normalized.contains("get_formatted_with_flags(key,effective_key_path,wire_fmt,force,wait"),
        "run_get's multi-key server-side loop must forward force and wait via \
         get_formatted_with_flags"
    );
}
