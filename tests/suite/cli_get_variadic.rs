use crate::common::daemon::isolate;
use crate::common::fetch::fetch;
use beachcomber::cache::Cache;
use beachcomber::cache::Schedule;
use beachcomber::provider::Value;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::server::Server;
use libbeachcomber::{ClientConfig, CombResult, Session};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

/// A `libbeachcomber` session on `sock`, with no daemon auto-start.
fn session_for(sock: std::path::PathBuf) -> Session {
    libbeachcomber::Client::with_config(ClientConfig {
        timeout: Duration::from_secs(2),
        auto_start: false,
        exec: false,
    })
    .with_socket_path(sock)
    .session()
    .unwrap()
}

/// The `data` of a hit; anything else is a panic naming `what`.
fn hit(result: CombResult, what: &str) -> serde_json::Value {
    match result {
        CombResult::Hit { data, .. } => data.as_value().clone(),
        CombResult::Template { .. } => panic!("{what}: expected a hit, got a template"),
        CombResult::Miss => panic!("{what}: expected a hit, got a miss"),
    }
}

async fn setup_server() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(ProviderRegistry::with_defaults());

    let mut hostname_fields = HashMap::new();
    hostname_fields.insert("name".to_string(), Value::String("myhost".to_string()));
    cache.put_source(
        "hostname",
        None,
        "host",
        hostname_fields,
        Schedule::poll(60, 1),
    );

    let mut user_fields = HashMap::new();
    user_fields.insert("name".to_string(), Value::String("alice".to_string()));
    cache.put_source("user", None, "name", user_fields, Schedule::poll(60, 1));

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    server.spawn().unwrap();

    (tmp, sock)
}

/// Fetch multiple keys on one session, returning the hits in order.
fn fetch_keys(session: &mut Session, keys: &[&str]) -> Vec<serde_json::Value> {
    keys.iter()
        .map(|key| hit(session.get(key, None).unwrap(), key))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_variadic_both_keys_succeed() {
    let (_tmp, sock) = setup_server().await;
    let mut session = session_for(sock);

    let values = fetch_keys(&mut session, &["hostname.name", "user.name"]);

    assert_eq!(values, vec!["myhost", "alice"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_variadic_partial_failure_still_emits_successful_key() {
    let (_tmp, sock) = setup_server().await;
    let mut session = session_for(sock);

    // "hostname.name" exists; "nonexistent.field" does not.
    let good = hit(session.get("hostname.name", None).unwrap(), "hostname.name");
    let bad = session.get("nonexistent.field", None);

    assert_eq!(good, "myhost");

    // The unknown key is a miss, and the connection stays open for the
    // remaining keys.
    match bad {
        Ok(CombResult::Miss) => {}
        other => panic!("expected a miss, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_variadic_single_key_is_unchanged() {
    // Single-key on a session must behave identically to the CLI's transport.
    let (_tmp, sock) = setup_server().await;

    let direct = fetch(&sock, "hostname.name", None).unwrap().unwrap();
    assert_eq!(direct.data.as_ref().unwrap(), "myhost");

    let mut session = session_for(sock);
    let via_session = hit(session.get("hostname.name", None).unwrap(), "hostname.name");
    assert_eq!(via_session, "myhost");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_variadic_json_format_produces_array() {
    // `comb get -f json a b` is one JSON array of the resolved values, in key
    // order.
    let (tmp, sock) = setup_server().await;

    let stdout = tokio::task::spawn_blocking(move || {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_comb"));
        isolate(&mut cmd, tmp.path(), &tmp.path().join("cfg"), &sock).args([
            "get",
            "-f",
            "json",
            "hostname.name",
            "user.name",
        ]);
        let out = assert_cmd::Command::from_std(cmd).assert().success();
        String::from_utf8(out.get_output().stdout.clone()).unwrap()
    })
    .await
    .unwrap();

    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON array");
    assert_eq!(parsed, serde_json::json!(["myhost", "alice"]));
}
