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

use libbeachcomber::{CombResult, IntrospectResponse, IntrospectSubject};

#[test]
fn put_with_data_then_get_round_trips() {
    let (_tmp, sock) = spawn_daemon();
    let client = client_for(&sock);

    client
        .put(
            "phase3_put",
            serde_json::json!({"color": "purple", "count": 3}),
            None,
            None,
        )
        .expect("put");

    match client.get("phase3_put.color", None).expect("get") {
        CombResult::Hit { data, .. } => {
            assert_eq!(data.as_text().as_deref(), Some("purple"));
        }
        CombResult::Miss => panic!("put followed by get should not miss"),
    }
}

#[test]
fn introspect_daemon_returns_typed_health() {
    let (_tmp, sock) = spawn_daemon();
    let client = client_for(&sock);
    let resp = client
        .introspect(IntrospectSubject::Daemon, None)
        .expect("introspect");
    match resp {
        IntrospectResponse::Daemon(health) => {
            assert!(health.pid > 0, "pid must be positive, got {}", health.pid);
            assert!(!health.version.is_empty());
        }
        IntrospectResponse::Other(_) => panic!("daemon subject must return Daemon variant"),
    }
}

#[test]
fn introspect_providers_returns_other_variant() {
    let (_tmp, sock) = spawn_daemon();
    let client = client_for(&sock);
    let resp = client
        .introspect(IntrospectSubject::Providers, None)
        .expect("introspect providers");
    match resp {
        IntrospectResponse::Other(_) => {}
        IntrospectResponse::Daemon(_) => panic!("providers must return Other variant"),
    }
}
