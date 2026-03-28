use shellstate::client::Client;
use shellstate::config::Config;
use shellstate::daemon;
use tempfile::TempDir;

#[tokio::test]
async fn e2e_all_builtin_providers_registered() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let config = Config::default();

    let handle = daemon::start_in_process(sock.clone(), config);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let client = Client::new(sock);

    let hostname = client.get("hostname", None).await.unwrap();
    assert!(hostname.ok && hostname.data.is_some(), "hostname should be cached");

    let user = client.get("user", None).await.unwrap();
    assert!(user.ok && user.data.is_some(), "user should be cached");

    let load_poke = client.poke("load", None).await.unwrap();
    assert!(load_poke.ok, "load poke should succeed");

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let load = client.get("load", None).await.unwrap();
    assert!(load.ok && load.data.is_some(), "load should have data after poke");

    handle.abort();
}

#[tokio::test]
async fn e2e_script_provider_from_config() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");

    let mut config = Config::default();
    config.providers.insert(
        "test_echo".to_string(),
        shellstate::config::ScriptProviderConfig {
            provider_type: Some("script".to_string()),
            command: r#"echo '{"greeting":"hello"}'"#.to_string(),
            ..Default::default()
        },
    );

    let handle = daemon::start_in_process(sock.clone(), config);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let client = Client::new(sock);

    let poke = client.poke("test_echo", None).await.unwrap();
    assert!(poke.ok, "Poke script provider should succeed");

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let result = client.get("test_echo.greeting", None).await.unwrap();
    assert!(result.ok, "Should get script provider result");
    assert_eq!(result.data.unwrap(), serde_json::json!("hello"));

    handle.abort();
}
