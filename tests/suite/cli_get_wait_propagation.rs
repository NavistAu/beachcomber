// Locks in wait=true semantics: at the `libbeachcomber` client layer
// (`Client::get_formatted_with_flags` and `Session::get_with_flags`) and at
// the binary (`comb get --wait`), against one stale seeded entry — a value
// served with wait=true must be the re-executed one, never the stale seed.
//
// run_get hands `wait` to `libbeachcomber::resolve` through `Inputs`, which
// forwards it on every fetch (pinned in libbeachcomber/tests/suite/resolve.rs);
// tests/suite/cli_get_multikey_force.rs runs the binary with `--force` the same way.
//
// Phase 1 Round 3 bug: run_get captured `wait` then discarded it via `_wait`,
// so every CLI --wait invocation silently behaved as wait=false.

use crate::common::daemon::isolate;
use beachcomber::cache::Cache;
use beachcomber::provider::Value;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::server::Server;
use libbeachcomber::{Client, ClientConfig, CombResult};
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
    cache.put_source("hostname", None, "host", fields, Some(0));

    // The entry must reach elapsed().as_secs() > 0 to count as stale, i.e. a
    // full second of clock. Both tests run on a paused clock (`start_paused`):
    // advance past the threshold and resume, and the seeded entry is a second
    // old against a normal running clock without a second of wall time.
    // Pausing needs the current-thread runtime, so the blocking `Client` calls
    // in the tests go through `spawn_blocking` — that leaves this runtime's one
    // thread free to drive the server task while a call is in flight.
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    tokio::time::resume();

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    // `spawn` binds the socket synchronously — it is connectable on return.
    server.spawn().expect("server binds socket");
    (tmp, sock)
}

#[tokio::test(start_paused = true)]
async fn single_key_text_wait_true_re_executes_stale_entry() {
    // wait=true on a stale entry: the seeded value is evicted and the live
    // provider runs inline — the returned text must not contain the seed.
    let (_tmp, sock) = setup_stale().await;

    let text = tokio::task::spawn_blocking(move || {
        client_for(sock)
            .get_formatted_with_flags("hostname.name", None, "text", false, true)
            .unwrap()
    })
    .await
    .expect("client call");

    assert!(
        !text.contains("stale-seed"),
        "wait=true on a stale entry must trigger re-execution; got {text:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn comb_get_wait_re_executes_stale_entry() {
    // The binary: `--wait` reaches the fetch, so the stale seed is not what
    // prints. A `wait: false` typo in run_get would serve "stale-seed".
    let (tmp, sock) = setup_stale().await;

    let stdout = tokio::task::spawn_blocking(move || {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_comb"));
        isolate(&mut cmd, tmp.path(), &tmp.path().join("cfg"))
            .env("BEACHCOMBER_SOCKET", &sock)
            .env("RUST_LOG", "error")
            .args(["get", "--wait", "hostname.name"]);
        let out = assert_cmd::Command::from_std(cmd).assert().success();
        String::from_utf8(out.get_output().stdout.clone()).unwrap()
    })
    .await
    .expect("comb runs");

    assert!(
        !stdout.contains("stale-seed"),
        "comb get --wait on a stale entry must trigger re-execution; got {stdout:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn multi_key_client_side_wait_true_re_executes_stale_entry() {
    // The same through a session's get_with_flags, wait=true forwarded.
    let (_tmp, sock) = setup_stale().await;

    let result = tokio::task::spawn_blocking(move || {
        let mut session = client_for(sock).session().unwrap();
        session
            .get_with_flags("hostname.name", None, false, true)
            .unwrap()
    })
    .await
    .expect("client call");

    let CombResult::Hit { data, age_ms, .. } = result else {
        panic!("expected a hit, got {result:?}");
    };
    let name = data.as_value().as_str().expect("name field is a string");
    assert_ne!(
        name, "stale-seed",
        "wait=true on a stale entry must trigger re-execution; got {name:?}"
    );
    assert_eq!(age_ms, 0, "wait re-execution must return age_ms=0");
}
