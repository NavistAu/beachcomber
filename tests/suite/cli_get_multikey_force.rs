// Verify that a multi-key `comb get` propagates force=true and wait=true to
// every fetch, and that the resolver's `Fetch` over a `libbeachcomber::Session`
// puts both on the wire. Phase 1 Round 3 bug: the CLI's session wrapper
// dropped both.

use crate::common::daemon::isolate;
use beachcomber::cache::Cache;
use beachcomber::cache::Schedule;
use beachcomber::provider::Value;
use beachcomber::server::Server;
use libbeachcomber::resolve::Fetch;
use libbeachcomber::{Client, ClientConfig};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

/// A `libbeachcomber` client on `sock`, with no daemon auto-start.
fn client_for(sock: std::path::PathBuf) -> Client {
    Client::with_config(ClientConfig {
        timeout: Duration::from_secs(2),
        auto_start: false,
        exec: false,
    })
    .with_socket_path(sock)
}

async fn setup_seeded() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(crate::common::registry::with_shipped_defaults());

    // Fresh seed with 60s interval — force must evict this.
    let mut fields = HashMap::new();
    fields.insert(
        "name".to_string(),
        Value::String("seeded-value".to_string()),
    );
    fields.insert("short".to_string(), Value::String("seeded".to_string()));
    cache.put_source("hostname", None, "host", fields, Schedule::poll(60, 1));

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    server.spawn().unwrap();
    (tmp, sock)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_fetch_forwards_force() {
    // force=true must evict a non-stale seeded entry, returning the live
    // provider value.
    let (_tmp, sock) = setup_seeded().await;
    let mut session = client_for(sock).session().unwrap();

    let text = Fetch::get(&mut session, "hostname.name", None, true, false)
        .unwrap()
        .expect("a hit")
        .data
        .expect("data")
        .to_string();

    assert!(
        !text.contains("seeded-value"),
        "force=true must evict the fresh seed and re-execute; got {text:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_fetch_forwards_wait() {
    // Fetching with wait=true on a fresh entry must
    // not error. This locks in that wait is an accepted parameter; the
    // behavioral wait=true semantics (stale-re-execute) are covered in
    // tests/suite/cli_get_wait_propagation.rs and tests/suite/get_wait.rs.
    let (_tmp, sock) = setup_seeded().await;
    let mut session = client_for(sock).session().unwrap();

    let text = Fetch::get(&mut session, "hostname.name", None, false, true)
        .unwrap()
        .expect("a hit")
        .data
        .expect("data")
        .to_string();

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
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_comb"));
        isolate(&mut cmd, tmp.path(), &tmp.path().join("cfg"), &sock).args([
            "get",
            "--force",
            "hostname.name",
            "hostname.short",
        ]);
        let out = assert_cmd::Command::from_std(cmd).assert().success();
        String::from_utf8(out.get_output().stdout.clone()).unwrap()
    })
    .await
    .unwrap();

    assert!(
        !stdout.contains("seeded"),
        "--force must evict every seeded field and re-execute; got {stdout:?}"
    );
}
