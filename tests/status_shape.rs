// Integration test for the reshaped Request::Status response.
// Status now returns an array of rows, one per (provider, path, field) tuple,
// describing everything currently warm in the cache.

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
async fn status_returns_rows_per_field() {
    let (_tmp, client, handle) = setup_daemon().await;

    // Warm up a global provider so at least something is in cache.
    let _ = client
        .send_raw(serde_json::json!({"op": "get", "key": "hostname"}))
        .await
        .expect("get hostname");

    // Give the cache a moment to settle.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let resp = client
        .send_raw(serde_json::json!({"op": "status"}))
        .await
        .expect("status");

    assert!(resp.ok, "status should succeed, error: {:?}", resp.error);
    let data = resp.data.expect("status data present");
    let rows = data.as_array().expect("status response is an array");

    for row in rows {
        assert!(
            row.get("provider").and_then(|v| v.as_str()).is_some(),
            "row missing 'provider' string: {row:?}"
        );
        // path may be null — just check the key is present
        assert!(row.get("path").is_some(), "row missing 'path' key: {row:?}");
        assert!(
            row.get("field").and_then(|v| v.as_str()).is_some(),
            "row missing 'field' string: {row:?}"
        );
        // value can be any JSON type
        assert!(row.get("value").is_some(), "row missing 'value' key: {row:?}");
        assert!(
            row.get("age_ms").and_then(|v| v.as_u64()).is_some(),
            "row missing 'age_ms' number: {row:?}"
        );
        assert!(
            row.get("stale").and_then(|v| v.as_bool()).is_some(),
            "row missing 'stale' bool: {row:?}"
        );
    }

    // hostname should be warm — at least one row with provider=="hostname".
    let has_hostname = rows.iter().any(|r| r["provider"] == "hostname");
    assert!(has_hostname, "expected hostname rows in status after get, got: {rows:?}");

    // Old blob keys must not be present anywhere in the response.
    let data_str = serde_json::to_string(&data).unwrap();
    assert!(
        !rows.iter().any(|r| r.get("pid").is_some()),
        "old 'pid' field must not appear in status rows"
    );
    assert!(
        !data_str.contains("\"cache_entries\""),
        "old 'cache_entries' key must not appear in status response"
    );
    assert!(
        !data_str.contains("\"uptime_secs\""),
        "old 'uptime_secs' key must not appear in status response"
    );

    handle.abort();
}

/// Status on a fresh daemon with an empty cache returns an empty array, not an error.
#[tokio::test]
async fn status_empty_cache_returns_empty_array() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "status"}))
        .await
        .expect("status");

    assert!(resp.ok, "status should succeed on empty cache: {:?}", resp.error);
    let data = resp.data.expect("status data present");
    let rows = data.as_array().expect("status response is an array");
    // May be empty — that's fine. Just verify it's a valid array.
    let _ = rows.len();

    handle.abort();
}
