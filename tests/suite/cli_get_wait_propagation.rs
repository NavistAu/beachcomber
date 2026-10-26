// Locks in `wait` semantics: at the `libbeachcomber` client layer
// (the resolver's `Fetch` over a `Client` and a `Session`) and at
// the binary (`comb get --wait forever`), against one stale seeded entry — a
// value served with `wait: forever` must be the re-executed one, never the
// stale seed. The bare `--wait` is a usage error.
//
// run_get hands `wait` to `libbeachcomber::resolve` through `Inputs`, which
// forwards it on every fetch (pinned in libbeachcomber/tests/suite/resolve.rs);
// tests/suite/cli_get_multikey_force.rs runs the binary with `--force` the same way.
//
// Phase 1 Round 3 bug: run_get captured `wait` then discarded it via `_wait`,
// so every CLI --wait invocation silently behaved as no wait at all.

use crate::common::daemon::isolate;
use beachcomber::cache::Cache;
use beachcomber::cache::Schedule;
use beachcomber::provider::Value;
use beachcomber::server::Server;
use libbeachcomber::resolve::Fetch;
use libbeachcomber::{Client, ClientConfig, Wait};
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
    let registry = Arc::new(crate::common::registry::with_shipped_defaults());

    // Seed hostname with a stale entry. interval=0 => stale after 0 elapsed secs.
    let mut fields = HashMap::new();
    fields.insert("name".to_string(), Value::String("stale-seed".to_string()));
    fields.insert("short".to_string(), Value::String("stale".to_string()));
    cache.put_source("hostname", None, "host", fields, Schedule::poll(0, 1));

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
async fn single_key_text_wait_forever_re_executes_stale_entry() {
    // `wait: forever` on a stale entry: the live provider runs and the read
    // waits for it — the returned text must not contain the seed.
    let (_tmp, sock) = setup_stale().await;

    let got = tokio::task::spawn_blocking(move || {
        Fetch::get(
            &mut &client_for(sock),
            "hostname.name",
            None,
            false,
            Wait::Forever,
        )
        .unwrap()
        .expect("a hit")
    })
    .await
    .expect("client call");

    let text = got.data.expect("data").to_string();
    assert!(
        !text.contains("stale-seed"),
        "wait forever on a stale entry must trigger re-execution; got {text:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn comb_get_wait_re_executes_stale_entry() {
    // The binary: `--wait forever` reaches the fetch, so the stale seed is not
    // what prints. A dropped `wait` in run_get would serve "stale-seed".
    let (tmp, sock) = setup_stale().await;

    let stdout = tokio::task::spawn_blocking(move || {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_comb"));
        isolate(&mut cmd, tmp.path(), &tmp.path().join("cfg"), &sock).args([
            "get",
            "--wait",
            "forever",
            "hostname.name",
        ]);
        let out = assert_cmd::Command::from_std(cmd).assert().success();
        String::from_utf8(out.get_output().stdout.clone()).unwrap()
    })
    .await
    .expect("comb runs");

    assert!(
        !stdout.contains("stale-seed"),
        "comb get --wait forever on a stale entry must trigger re-execution; got {stdout:?}"
    );
}

/// `--wait` takes a duration or `forever`: the bare flag, and a value that is
/// neither, are usage errors that reach no daemon.
#[test]
fn a_bare_wait_is_a_usage_error() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("nobody.sock");
    for (args, says) in [
        (
            &["get", "hostname.name", "--wait"][..],
            "a value is required for '--wait <DURATION|forever>'",
        ),
        (
            &["get", "--wait", "soon", "hostname.name"][..],
            "invalid wait `soon`",
        ),
    ] {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_comb"));
        isolate(&mut cmd, tmp.path(), &tmp.path().join("cfg"), &sock).args(args);
        let out = cmd.output().expect("comb runs");
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(says), "{args:?}: {stderr}");
    }
}

#[tokio::test(start_paused = true)]
async fn multi_key_client_side_wait_forever_re_executes_stale_entry() {
    // The same through a session's Fetch, `wait: forever` forwarded.
    let (_tmp, sock) = setup_stale().await;

    let got = tokio::task::spawn_blocking(move || {
        let mut session = client_for(sock).session().unwrap();
        Fetch::get(&mut session, "hostname.name", None, false, Wait::Forever)
            .unwrap()
            .expect("a hit")
    })
    .await
    .expect("client call");

    let age_ms = got.age_ms.expect("a hit carries its age");
    let data = got.data.expect("data");
    let name = data.as_str().expect("name field is a string");
    assert_ne!(
        name, "stale-seed",
        "wait forever on a stale entry must trigger re-execution; got {name:?}"
    );
    // The seed is two seconds old; a re-execution is as old as this call took.
    assert!(
        age_ms < 1000,
        "wait re-execution must return a fresh record, not the seed's age: {age_ms}ms"
    );
}
