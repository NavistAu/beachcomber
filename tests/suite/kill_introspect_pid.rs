// Regression: comb kill must resolve the daemon's PID via introspect{daemon}.
// Previously it queried {"op":"status"} and read data.pid, which broke when
// Status was changed to return cache rows.

use beachcomber::cache::Cache;
use beachcomber::server::Server;
use std::sync::Arc;
use tempfile::TempDir;

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
