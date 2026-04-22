// Regression: comb kill must resolve the daemon's PID via introspect{daemon}.
// Previously it queried {"op":"status"} and read data.pid, which broke when
// Status was changed to return cache rows.

use beachcomber::cache::Cache;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::server::Server;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[tokio::test]
async fn introspect_daemon_returns_current_pid() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(ProviderRegistry::with_defaults());

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    writer
        .write_all(b"{\"op\":\"introspect\",\"subject\":\"daemon\"}\n")
        .await
        .unwrap();

    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(parsed["ok"], serde_json::json!(true), "introspect{{daemon}} must succeed: {:?}", parsed);
    let pid = parsed["data"]["pid"]
        .as_i64()
        .expect("introspect{daemon} response must include data.pid as i64");
    assert_eq!(
        pid as u32,
        std::process::id(),
        "introspect{{daemon}} must return the current process PID"
    );
}

#[tokio::test]
async fn status_op_does_not_contain_pid_field() {
    // Locks in the current schema: Status returns cache rows (an array), not
    // a daemon-health object. This documents the contract that motivated the
    // comb kill pid-lookup fix above — comb kill can no longer read a pid
    // field out of Status because the shape is now an array.
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(ProviderRegistry::with_defaults());

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    writer.write_all(b"{\"op\":\"status\"}\n").await.unwrap();

    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(parsed["ok"], serde_json::json!(true));
    assert!(
        parsed["data"].is_array(),
        "status data must be an array of cache rows, got {:?}",
        parsed["data"]
    );
}

#[test]
fn query_daemon_pid_request_string_is_exact() {
    // Contract: query_daemon_pid sends this exact line. If main.rs drifts
    // back to the old "status" op, grep will catch it, but this test locks
    // the expectation in a machine-checkable form.
    let src = std::fs::read_to_string("src/main.rs").unwrap();
    assert!(
        src.contains(r#"{\"op\":\"introspect\",\"subject\":\"daemon\"}"#),
        "src/main.rs must send introspect{{daemon}} from query_daemon_pid"
    );
    // The old behavior must be gone from the PID-lookup path. query_daemon_pid
    // is the only site in main.rs that sends a raw wire op as a byte literal;
    // if status is still there in that context it's a regression.
    let op_status_bytes = r#"b"{\"op\":\"status\"}\n""#;
    assert!(
        !src.contains(op_status_bytes),
        "src/main.rs must no longer send the literal {op_status_bytes} \
         (old comb kill pid lookup)"
    );
}
