use beachcomber::client::Client;
use beachcomber::daemon;
use tempfile::TempDir;

use crate::common::fetch::fetch;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_get_hostname_via_daemon() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let config = crate::common::registry::config_over_shipped(vec![]);

    let (handle, ready) = daemon::start_in_process_polling(sock.clone(), config);
    ready.await.expect("daemon ready");

    let response = fetch(&sock, "hostname", None)
        .unwrap()
        .expect("Response should be ok");
    let data = response.data.unwrap();
    assert!(data["name"].is_string(), "hostname.name should be a string");
    assert!(
        !data["name"].as_str().unwrap().is_empty(),
        "hostname should not be empty"
    );

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_get_hostname_single_field() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let config = crate::common::registry::config_over_shipped(vec![]);

    let (handle, ready) = daemon::start_in_process_polling(sock.clone(), config);
    ready.await.expect("daemon ready");

    let response = fetch(&sock, "hostname.short", None).unwrap().unwrap();
    let short = response.data.unwrap();
    assert!(
        short.is_string(),
        "Single field should return a string value"
    );

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_get_user() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let config = crate::common::registry::config_over_shipped(vec![]);

    let (handle, ready) = daemon::start_in_process_polling(sock.clone(), config);
    ready.await.expect("daemon ready");

    let response = fetch(&sock, "user", None).unwrap().unwrap();
    let data = response.data.unwrap();
    assert!(data["name"].is_string());
    assert!(data["uid"].is_number());

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_refresh_and_get() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let config = crate::common::registry::config_over_shipped(vec![]);

    let (handle, ready) = daemon::start_in_process_polling(sock.clone(), config);
    ready.await.expect("daemon ready");

    let client = Client::new(sock.clone());

    let refresh_resp = client.refresh("hostname", None).unwrap();
    assert!(refresh_resp.ok);

    let response = fetch(&sock, "hostname.name", None).unwrap().unwrap();
    assert!(
        response.age_ms.unwrap() < 1000,
        "Data should be fresh after refresh"
    );

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_unknown_provider() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let config = crate::common::registry::config_over_shipped(vec![]);

    let (handle, ready) = daemon::start_in_process_polling(sock.clone(), config);
    ready.await.expect("daemon ready");

    let response = fetch(&sock, "nonexistent", None);
    assert!(
        matches!(response, Ok(None)),
        "an unknown provider is a miss, got {response:?}"
    );

    handle.abort();
}

// The 10 concurrent sync `Client` calls block, so they run on the blocking
// pool (spawn_blocking), never on runtime workers — otherwise, on a machine
// with fewer cores than clients (CI runners), the blockers occupy every
// worker and the in-process daemon task starves: a real deadlock, observed
// as a 30s test timeout on 2-4-core CI. With the blockers off the workers,
// this test pins worker_threads = 2 like its siblings.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_multiple_concurrent_clients() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let config = crate::common::registry::config_over_shipped(vec![]);

    let (handle, ready) = daemon::start_in_process_polling(sock.clone(), config);
    ready.await.expect("daemon ready");

    let mut handles = Vec::new();
    for _ in 0..10 {
        let sock = sock.clone();
        handles.push(tokio::task::spawn_blocking(move || {
            let response = fetch(&sock, "hostname.name", None).unwrap().unwrap();
            response.data.unwrap().as_str().unwrap().to_string()
        }));
    }

    let mut results = Vec::new();
    for h in handles {
        results.push(h.await.unwrap());
    }

    let first = &results[0];
    for r in &results {
        assert_eq!(
            r, first,
            "All concurrent clients should get the same hostname"
        );
    }

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_introspect_includes_pid_and_version() {
    // pid and version moved from Request::Status to Request::Introspect{daemon}.
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let config = crate::common::registry::config_over_shipped(vec![]);

    let (handle, ready) = daemon::start_in_process_polling(sock.clone(), config);
    ready.await.expect("daemon ready");

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut stream = tokio::net::UnixStream::connect(&sock).await.unwrap();
    stream
        .write_all(b"{\"op\":\"introspect\",\"subject\":\"daemon\"}\n")
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(parsed["ok"], true, "introspect daemon should succeed");

    let pid = parsed["data"]["pid"]
        .as_i64()
        .expect("introspect daemon response should include pid");
    assert_eq!(
        pid as u32,
        std::process::id(),
        "in-process daemon reports the test process pid",
    );

    let version = parsed["data"]["version"]
        .as_str()
        .expect("introspect daemon response should include version");
    assert_eq!(version, env!("BEACHCOMBER_VERSION"));

    handle.abort();
}
