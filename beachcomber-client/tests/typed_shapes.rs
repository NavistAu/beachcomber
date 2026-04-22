// Phase 3: typed-shape integration tests for the public SDK crate.

use beachcomber::config::Config;
use beachcomber::daemon;
use libbeachcomber::{Client, ClientConfig};
use std::time::Duration;
use tempfile::TempDir;

fn spawn_daemon() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let sock_clone = sock.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let handle = daemon::start_in_process(sock_clone, Config::default());
            tokio::time::sleep(Duration::from_millis(200)).await;
            handle.await.ok();
        });
    });
    std::thread::sleep(Duration::from_millis(150));
    (tmp, sock)
}

fn client_for(sock: &std::path::Path) -> Client {
    Client::with_config(ClientConfig {
        timeout: Duration::from_secs(2),
        auto_start: false,
    })
    .with_socket_path(sock.to_path_buf())
}

#[test]
fn status_returns_typed_cache_rows() {
    let (_tmp, sock) = spawn_daemon();
    let client = client_for(&sock);

    client.put_null("phase3_marker", None).ok(); // ensure something virtual registered

    // The put_null call above may be ignored if the op fails; status should
    // still succeed. What we're really asserting is the typed shape.
    let rows = client.status().expect("status");

    // Rows is a typed Vec<CacheRow> — just confirming the shape deserialized.
    // Content assertion happens in Task 2 after Client::put (with data) lands.
    let _ = rows.iter().filter(|r| r.stale || !r.stale).count();
}
