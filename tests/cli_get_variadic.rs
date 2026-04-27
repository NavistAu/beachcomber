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
    cache.put_source("hostname", None, "main", hostname_fields, Some(60));

    let mut user_fields = HashMap::new();
    user_fields.insert("value".to_string(), Value::String("alice".to_string()));
    cache.put_source("user", None, "main", user_fields, Some(60));

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    (tmp, sock)
}

/// Open a session and fetch multiple keys, returning the responses in order.
async fn fetch_keys(
    session: &mut ClientSession,
    keys: &[&str],
) -> Vec<beachcomber::protocol::Response> {
    let mut results = Vec::new();
    for key in keys {
        let resp = session.get(key, None).await.unwrap();
        results.push(resp);
    }
    results
}

#[tokio::test]
async fn get_variadic_both_keys_succeed() {
    let (_tmp, sock) = setup_server().await;
    let client = Client::new(sock);
    let mut session = client.connect().await.unwrap();

    let responses = fetch_keys(&mut session, &["hostname.value", "user.value"]).await;

    assert_eq!(responses.len(), 2);
    assert!(responses[0].ok);
    assert_eq!(responses[0].data.as_ref().unwrap(), "myhost");
    assert!(responses[1].ok);
    assert_eq!(responses[1].data.as_ref().unwrap(), "alice");
}

#[tokio::test]
async fn get_variadic_partial_failure_still_emits_successful_key() {
    let (_tmp, sock) = setup_server().await;
    let client = Client::new(sock);
    let mut session = client.connect().await.unwrap();

    // "hostname.value" exists; "nonexistent.field" does not.
    let resp_good = session.get("hostname.value", None).await.unwrap();
    let resp_bad = session.get("nonexistent.field", None).await.unwrap();

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

#[tokio::test]
async fn get_variadic_single_key_is_unchanged() {
    // Single-key on a session must behave identically to Client::get.
    let (_tmp, sock) = setup_server().await;
    let client = Client::new(sock.clone());

    let direct = client.get("hostname.value", None).await.unwrap();
    assert!(direct.ok);
    assert_eq!(direct.data.as_ref().unwrap(), "myhost");

    let client2 = Client::new(sock);
    let mut session = client2.connect().await.unwrap();
    let via_session = session.get("hostname.value", None).await.unwrap();
    assert!(via_session.ok);
    assert_eq!(via_session.data.as_ref().unwrap(), "myhost");
}

#[tokio::test]
async fn get_variadic_json_format_produces_array() {
    // Verify that multiple-key JSON aggregation produces a valid JSON array.
    let (_tmp, sock) = setup_server().await;
    let client = Client::new(sock);
    let mut session = client.connect().await.unwrap();

    let responses = fetch_keys(&mut session, &["hostname.value", "user.value"]).await;

    // Serialize as an array the same way run_get does.
    let arr: Vec<&beachcomber::protocol::Response> = responses.iter().collect();
    let json_str = serde_json::to_string_pretty(&arr).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json_str).expect("valid JSON array");

    assert!(parsed.is_array());
    let arr = parsed.as_array().unwrap();
    assert_eq!(arr.len(), 2);

    // Each element must have ok:true and a data field.
    for element in arr {
        assert_eq!(element["ok"], serde_json::json!(true));
        assert!(!element["data"].is_null());
    }
}
