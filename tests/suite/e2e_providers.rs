use crate::common::fetch::fetch;
use crate::common::poll::{poll_until, poll_until_eq};
use beachcomber::client::Client;
use beachcomber::daemon;
use std::time::Duration;
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_all_builtin_providers_registered() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let config = crate::common::registry::config_over_shipped(vec![]);

    let (handle, ready) = daemon::start_in_process_polling(sock.clone(), config);
    ready.await.expect("daemon ready");

    let client = Client::new(sock.clone());

    let hostname = fetch(&sock, "hostname", None).unwrap();
    assert!(
        hostname.is_some_and(|got| got.data.is_some()),
        "hostname should be cached"
    );

    let user = fetch(&sock, "user", None).unwrap();
    assert!(
        user.is_some_and(|got| got.data.is_some()),
        "user should be cached"
    );

    let load_refresh = client.refresh("load", None).unwrap();
    assert!(load_refresh.ok, "load refresh should succeed");

    poll_until(
        "load after refresh",
        "a cached value",
        Duration::from_secs(5),
        Duration::from_millis(20),
        || fetch(&sock, "load", None).unwrap().and_then(|got| got.data),
        |data| data.is_some(),
    );

    let load = fetch(&sock, "load", None).unwrap();
    assert!(
        load.is_some_and(|got| got.data.is_some()),
        "load should have data after refresh"
    );

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_config_source_from_config() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");

    // An in-process daemon loads no managed defaults, so the Source says its
    // own schedule.
    let config = beachcomber::config::Config::from_toml(
        r#"
[daemon.providers.test_echo.sources.main]
data = "{{ exec(['echo', '{\"greeting\":\"hello\"}']) | fromjson }}"
result = "{{ data }}"
return = "{{ result | poll('60s') }}"
"#,
    );
    assert_eq!(config.errors(), Vec::<String>::new());

    let (handle, ready) = daemon::start_in_process_polling(sock.clone(), config);
    ready.await.expect("daemon ready");

    let client = Client::new(sock.clone());

    let refresh = client.refresh("test_echo", None).unwrap();
    assert!(refresh.ok, "Refresh config Source should succeed");

    poll_until_eq(
        "test_echo.greeting after refresh",
        Duration::from_secs(5),
        Duration::from_millis(20),
        || {
            fetch(&sock, "test_echo.greeting", None)
                .unwrap()
                .and_then(|got| got.data)
        },
        Some(serde_json::json!("hello")),
    );

    let result = fetch(&sock, "test_echo.greeting", None)
        .unwrap()
        .expect("Should get config Source result");
    assert_eq!(result.data.unwrap(), serde_json::json!("hello"));

    handle.abort();
}
