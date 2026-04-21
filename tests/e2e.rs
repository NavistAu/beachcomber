use beachcomber::client::Client;
use beachcomber::config::Config;
use beachcomber::daemon;
use tempfile::TempDir;

#[tokio::test]
async fn e2e_get_hostname_via_daemon() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let config = Config::default();

    let handle = daemon::start_in_process(sock.clone(), config);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = Client::new(sock.clone());
    let response = client.get("hostname", None).await.unwrap();
    assert!(response.ok, "Response should be ok");
    let data = response.data.unwrap();
    assert!(data["name"].is_string(), "hostname.name should be a string");
    assert!(
        !data["name"].as_str().unwrap().is_empty(),
        "hostname should not be empty"
    );

    handle.abort();
}

#[tokio::test]
async fn e2e_get_hostname_single_field() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let config = Config::default();

    let handle = daemon::start_in_process(sock.clone(), config);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = Client::new(sock.clone());
    let response = client.get("hostname.short", None).await.unwrap();
    assert!(response.ok);
    let short = response.data.unwrap();
    assert!(
        short.is_string(),
        "Single field should return a string value"
    );

    handle.abort();
}

#[tokio::test]
async fn e2e_get_hostname_text_format() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let config = Config::default();

    let handle = daemon::start_in_process(sock.clone(), config);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = Client::new(sock.clone());
    let text = client.get_text("hostname.short", None).await.unwrap();
    assert!(!text.is_empty(), "Text response should not be empty");
    assert!(!text.contains('{'), "Text format should not contain JSON");

    handle.abort();
}

#[tokio::test]
async fn e2e_get_user() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let config = Config::default();

    let handle = daemon::start_in_process(sock.clone(), config);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = Client::new(sock.clone());
    let response = client.get("user", None).await.unwrap();
    assert!(response.ok);
    let data = response.data.unwrap();
    assert!(data["name"].is_string());
    assert!(data["uid"].is_number());

    handle.abort();
}

#[tokio::test]
async fn e2e_refresh_and_get() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let config = Config::default();

    let handle = daemon::start_in_process(sock.clone(), config);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = Client::new(sock.clone());

    let refresh_resp = client.refresh("hostname", None).await.unwrap();
    assert!(refresh_resp.ok);

    let response = client.get("hostname.name", None).await.unwrap();
    assert!(response.ok);
    assert!(
        response.age_ms.unwrap() < 1000,
        "Data should be fresh after refresh"
    );

    handle.abort();
}

#[tokio::test]
async fn e2e_unknown_provider() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let config = Config::default();

    let handle = daemon::start_in_process(sock.clone(), config);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = Client::new(sock.clone());
    let response = client.get("nonexistent", None).await.unwrap();
    assert!(!response.ok, "Unknown provider should return error");

    handle.abort();
}

#[tokio::test]
async fn e2e_multiple_concurrent_clients() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let config = Config::default();

    let handle = daemon::start_in_process(sock.clone(), config);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut handles = Vec::new();
    for _ in 0..10 {
        let sock = sock.clone();
        handles.push(tokio::spawn(async move {
            let client = Client::new(sock);
            let response = client.get("hostname.name", None).await.unwrap();
            assert!(response.ok);
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

#[tokio::test]
async fn status_response_includes_pid_and_version() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let config = Config::default();

    let handle = daemon::start_in_process(sock.clone(), config);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Send a status request directly so we can inspect extra fields the typed
    // client doesn't expose.
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut stream = tokio::net::UnixStream::connect(&sock).await.unwrap();
    stream.write_all(b"{\"op\":\"status\"}\n").await.unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(parsed["ok"], true, "status request should succeed");

    let pid = parsed["data"]["pid"]
        .as_i64()
        .expect("status response should include pid");
    assert_eq!(
        pid as u32,
        std::process::id(),
        "in-process daemon reports the test process pid",
    );

    let version = parsed["data"]["version"]
        .as_str()
        .expect("status response should include version");
    assert_eq!(version, env!("CARGO_PKG_VERSION"));

    handle.abort();
}
