use shellstate::client::Client;
use shellstate::config::Config;
use shellstate::daemon;
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
    assert!(!data["name"].as_str().unwrap().is_empty(), "hostname should not be empty");

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
    assert!(short.is_string(), "Single field should return a string value");

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
async fn e2e_poke_and_get() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let config = Config::default();

    let handle = daemon::start_in_process(sock.clone(), config);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client = Client::new(sock.clone());

    let poke_resp = client.poke("hostname", None).await.unwrap();
    assert!(poke_resp.ok);

    let response = client.get("hostname.name", None).await.unwrap();
    assert!(response.ok);
    assert!(response.age_ms.unwrap() < 1000, "Data should be fresh after poke");

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
        assert_eq!(r, first, "All concurrent clients should get the same hostname");
    }

    handle.abort();
}
