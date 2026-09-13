use beachcomber::cache::Cache;
use beachcomber::client::{Client, ClientSession};
use beachcomber::provider::Value;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::server::Server;
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;

async fn setup_server() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(ProviderRegistry::with_defaults());

    let mut hostname_fields = HashMap::new();
    hostname_fields.insert("value".to_string(), Value::String("myhost".to_string()));
    cache.put_source("hostname", None, "host", hostname_fields, Some(60));

    let mut user_fields = HashMap::new();
    user_fields.insert("value".to_string(), Value::String("alice".to_string()));
    cache.put_source("user", None, "name", user_fields, Some(60));

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    server.spawn().unwrap();

    (tmp, sock)
}

/// Open a session and fetch multiple keys, returning the responses in order.
async fn fetch_keys(
    session: &mut ClientSession,
    keys: &[&str],
) -> Vec<beachcomber::protocol::Response> {
    let mut results = Vec::new();
    for key in keys {
        let resp = session.get(key, None).unwrap();
        results.push(resp);
    }
    results
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_variadic_both_keys_succeed() {
    let (_tmp, sock) = setup_server().await;
    let client = Client::new(sock);
    let mut session = client.connect().unwrap();

    let responses = fetch_keys(&mut session, &["hostname.value", "user.value"]).await;

    assert_eq!(responses.len(), 2);
    assert!(responses[0].ok);
    assert_eq!(responses[0].data.as_ref().unwrap(), "myhost");
    assert!(responses[1].ok);
    assert_eq!(responses[1].data.as_ref().unwrap(), "alice");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_variadic_partial_failure_still_emits_successful_key() {
    let (_tmp, sock) = setup_server().await;
    let client = Client::new(sock);
    let mut session = client.connect().unwrap();

    // "hostname.value" exists; "nonexistent.field" does not.
    let resp_good = session.get("hostname.value", None).unwrap();
    let resp_bad = session.get("nonexistent.field", None).unwrap();

    assert!(resp_good.ok);
    assert_eq!(resp_good.data.as_ref().unwrap(), "myhost");

    // The server returns ok:false for the missing key rather than closing the connection,
    // allowing the caller to continue fetching remaining keys.
    assert!(!resp_bad.ok);
    assert!(
        resp_bad
            .error
            .as_deref()
            .unwrap_or("")
            .contains("unknown provider")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_variadic_single_key_is_unchanged() {
    // Single-key on a session must behave identically to Client::get.
    let (_tmp, sock) = setup_server().await;
    let client = Client::new(sock.clone());

    let direct = client.get("hostname.value", None).unwrap();
    assert!(direct.ok);
    assert_eq!(direct.data.as_ref().unwrap(), "myhost");

    let client2 = Client::new(sock);
    let mut session = client2.connect().unwrap();
    let via_session = session.get("hostname.value", None).unwrap();
    assert!(via_session.ok);
    assert_eq!(via_session.data.as_ref().unwrap(), "myhost");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_variadic_json_format_produces_array() {
    // `comb get -f json a b` is one JSON array of the resolved values, in key
    // order.
    let (tmp, sock) = setup_server().await;

    let stdout = tokio::task::spawn_blocking(move || {
        let out = assert_cmd::Command::cargo_bin("comb")
            .unwrap()
            .env("BEACHCOMBER_SOCKET", &sock)
            .env("RUST_LOG", "error")
            .env("HOME", tmp.path())
            .env("XDG_CONFIG_HOME", tmp.path().join("cfg"))
            .args(["get", "-f", "json", "hostname.value", "user.value"])
            .assert()
            .success();
        String::from_utf8(out.get_output().stdout.clone()).unwrap()
    })
    .await
    .unwrap();

    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON array");
    assert_eq!(parsed, serde_json::json!(["myhost", "alice"]));
}
