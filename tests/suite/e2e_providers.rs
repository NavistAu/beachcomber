use crate::common::poll::{poll_until, poll_until_eq};
use beachcomber::client::Client;
use beachcomber::config::Config;
use beachcomber::daemon;
use std::time::Duration;
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_all_builtin_providers_registered() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let config = Config::default();

    let (handle, ready) = daemon::start_in_process_polling(sock.clone(), config);
    ready.await.expect("daemon ready");

    let client = Client::new(sock);

    let hostname = client.get("hostname", None).unwrap();
    assert!(
        hostname.ok && hostname.data.is_some(),
        "hostname should be cached"
    );

    let user = client.get("user", None).unwrap();
    assert!(user.ok && user.data.is_some(), "user should be cached");

    let load_refresh = client.refresh("load", None).unwrap();
    assert!(load_refresh.ok, "load refresh should succeed");

    poll_until(
        "load after refresh",
        "a cached value",
        Duration::from_secs(5),
        Duration::from_millis(20),
        || client.get("load", None).unwrap().data,
        |data| data.is_some(),
    );

    let load = client.get("load", None).unwrap();
    assert!(
        load.ok && load.data.is_some(),
        "load should have data after refresh"
    );

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_script_provider_from_config() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");

    let mut config = Config::default();
    // Insert a script provider using the toml::Value representation.
    let mut table = toml::Table::new();
    table.insert(
        "type".to_string(),
        toml::Value::String("script".to_string()),
    );
    table.insert(
        "command".to_string(),
        toml::Value::String(r#"echo '{"greeting":"hello"}'"#.to_string()),
    );
    config
        .providers
        .insert("test_echo".to_string(), toml::Value::Table(table));

    let (handle, ready) = daemon::start_in_process_polling(sock.clone(), config);
    ready.await.expect("daemon ready");

    let client = Client::new(sock);

    let refresh = client.refresh("test_echo", None).unwrap();
    assert!(refresh.ok, "Refresh script provider should succeed");

    poll_until_eq(
        "test_echo.greeting after refresh",
        Duration::from_secs(5),
        Duration::from_millis(20),
        || client.get("test_echo.greeting", None).unwrap().data,
        Some(serde_json::json!("hello")),
    );

    let result = client.get("test_echo.greeting", None).unwrap();
    assert!(result.ok, "Should get script provider result");
    assert_eq!(result.data.unwrap(), serde_json::json!("hello"));

    handle.abort();
}
