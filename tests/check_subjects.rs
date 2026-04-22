// Integration tests for the Request::Introspect per-subject payloads.
// Uses the in-process test daemon pattern from tests/put_null.rs.

use beachcomber::client::Client;
use beachcomber::config::Config;

async fn setup_daemon() -> (tempfile::TempDir, Client, tokio::task::JoinHandle<()>) {
    let tmp = tempfile::TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let config = Config::load();
    let handle = beachcomber::daemon::start_in_process(sock.clone(), config);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let client = Client::new(sock);
    (tmp, client, handle)
}

#[tokio::test]
async fn introspect_daemon_returns_expected_fields() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({
            "op": "introspect",
            "subject": "daemon"
        }))
        .await
        .expect("request succeeded");

    assert!(resp.ok, "error: {:?}", resp.error);
    let data = resp.data.expect("payload present");

    assert!(
        data.get("pid").and_then(|v| v.as_u64()).is_some(),
        "pid missing or not a number"
    );
    assert!(
        data.get("version").and_then(|v| v.as_str()).is_some(),
        "version missing or not a string"
    );
    assert!(
        data.get("uptime_secs").and_then(|v| v.as_u64()).is_some(),
        "uptime_secs missing or not a number"
    );
    assert!(
        data.get("socket_path").and_then(|v| v.as_str()).is_some(),
        "socket_path missing or not a string"
    );
    // config_path may be null but must be present
    assert!(data.get("config_path").is_some(), "config_path key absent");
    assert!(
        data.get("requests_total").and_then(|v| v.as_u64()).is_some(),
        "requests_total missing or not a number"
    );
    assert!(
        data.get("in_flight").and_then(|v| v.as_u64()).is_some(),
        "in_flight missing or not a number"
    );
    assert!(
        data.get("active_watchers").and_then(|v| v.as_u64()).is_some(),
        "active_watchers missing or not a number"
    );
    assert!(
        data.get("cache_entries").and_then(|v| v.as_u64()).is_some(),
        "cache_entries missing or not a number"
    );

    let verdicts = data
        .get("verdicts")
        .and_then(|v| v.as_array())
        .expect("verdicts array");
    assert!(!verdicts.is_empty(), "at least one verdict expected");
    for v in verdicts {
        let level = v
            .get("level")
            .and_then(|x| x.as_str())
            .expect("verdict has level");
        assert!(
            ["PASS", "WARN", "FAIL"].contains(&level),
            "unexpected verdict level: {level}"
        );
        assert!(
            v.get("message").and_then(|x| x.as_str()).is_some(),
            "verdict missing message"
        );
    }

    handle.abort();
}
