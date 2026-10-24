use crate::common::fetch::fetch;
use beachcomber::cache::Cache;
use beachcomber::cache::Schedule;
use beachcomber::client::Client;
use beachcomber::provider::Value;
use beachcomber::server::Server;
use libbeachcomber::resolve::FetchError;
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;

async fn setup_server() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(crate::common::registry::with_shipped_defaults());

    // Pre-populate cache
    let mut hostname = HashMap::new();
    hostname.insert(
        "name".to_string(),
        Value::String("testhost.local".to_string()),
    );
    hostname.insert("short".to_string(), Value::String("testhost".to_string()));
    cache.put_source("hostname", None, "host", hostname, Schedule::poll(60, 1));

    let mut user = HashMap::new();
    user.insert("name".to_string(), Value::String("testuser".to_string()));
    user.insert("uid".to_string(), Value::Int(501));
    cache.put_source("user", None, "name", user, Schedule::poll(60, 1));

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    server.spawn().unwrap();

    (tmp, sock)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_get_full_provider() {
    let (_tmp, sock) = setup_server().await;
    let response = fetch(&sock, "hostname", None).unwrap().unwrap();
    let data = response.data.unwrap();
    assert_eq!(data["name"], "testhost.local");
    assert_eq!(data["short"], "testhost");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_get_single_field() {
    let (_tmp, sock) = setup_server().await;
    let response = fetch(&sock, "hostname.short", None).unwrap().unwrap();
    assert_eq!(response.data.unwrap(), serde_json::json!("testhost"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_get_unknown_provider() {
    let (_tmp, sock) = setup_server().await;
    match fetch(&sock, "nonexistent", None) {
        Ok(None) => {}
        other => panic!("expected a miss, got {other:?}"),
    }
    match fetch(&sock, "a:b:c.x", None) {
        Err(FetchError::Rejected(msg)) => assert!(msg.contains("invalid facet key")),
        other => panic!("expected a rejection, got {other:?}"),
    }
}

/// The CLI transport keeps the template half: a template-only node is a hit
/// with `template` set and no `data`, never a bare miss.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_get_carries_the_template_half() {
    let (_tmp, sock) = setup_server().await;
    let client = Client::new(sock.clone());
    let resp = client
        .put(
            "tpl:template",
            serde_json::json!({"t": "{{ 1 }}"}),
            None,
            None,
        )
        .unwrap();
    assert!(resp.ok, "{:?}", resp.error);

    let resp = fetch(&sock, "tpl.t", None).unwrap().unwrap();
    assert!(resp.data.is_none(), "a template-only node has no data");
    assert_eq!(resp.template, Some(serde_json::json!("{{ 1 }}")));
    assert_eq!(resp.rest, None);

    let resp = fetch(&sock, "tpl.t.x", None).unwrap().unwrap();
    assert_eq!(
        resp.rest.as_deref(),
        Some("x"),
        "the address past the template string"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_refresh() {
    let (_tmp, sock) = setup_server().await;
    let client = Client::new(sock);
    let response = client.refresh("hostname", None).unwrap();
    assert!(response.ok);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_put_and_get() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let config = beachcomber::config::Config::default();
    let (handle, ready) = beachcomber::daemon::start_in_process_polling(sock.clone(), config);
    ready.await.expect("daemon ready");

    let client = beachcomber::client::Client::new(sock.clone());

    let resp = client
        .put("testapp", serde_json::json!({"status": "ok"}), None, None)
        .unwrap();
    assert!(resp.ok);

    let resp = fetch(&sock, "testapp.status", None).unwrap().unwrap();
    assert_eq!(resp.data.unwrap(), "ok");

    handle.abort();
}
