// Regression: comb kill must resolve the daemon's PID via introspect{daemon}.
// Previously it queried {"op":"status"} and read data.pid, which broke when
// Status was changed to return cache rows.

use beachcomber::cache::Cache;
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
    let registry = Arc::new(crate::common::registry::with_shipped_defaults());

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    server.spawn().unwrap();

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
    assert_eq!(
        parsed["ok"],
        serde_json::json!(true),
        "introspect{{daemon}} must succeed: {:?}",
        parsed
    );
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
    let registry = Arc::new(crate::common::registry::with_shipped_defaults());

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    server.spawn().unwrap();

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

/// `comb kill` asks the daemon for its pid through the client's
/// `introspect{daemon}`: the pid it reads is the serving process's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_reads_the_pid_the_daemon_reports() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(crate::common::registry::with_shipped_defaults());
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    server.spawn().unwrap();

    let pid = tokio::task::spawn_blocking(move || {
        beachcomber::cli::commands::kill::resolve_daemon_pid(&sock)
    })
    .await
    .unwrap()
    .expect("the daemon reports its pid");
    assert_eq!(pid as u32, std::process::id());
}
