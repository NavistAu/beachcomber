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

#[tokio::test]
async fn introspect_providers_lists_catalog_with_scope_and_fields() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "providers"}))
        .await
        .expect("providers introspect");
    assert!(resp.ok, "error: {:?}", resp.error);
    let data = resp.data.expect("payload present");

    let providers = data.get("providers").and_then(|v| v.as_array()).expect("providers array");
    assert!(!providers.is_empty(), "at least one provider registered");

    // Check a known global provider like hostname
    let hostname = providers.iter().find(|p| p["name"] == "hostname");
    if let Some(h) = hostname {
        assert_eq!(h["source"].as_str(), Some("builtin"));
        assert_eq!(h["scope"].as_str(), Some("global"));
        assert!(h.get("fields").and_then(|v| v.as_array()).is_some());
        assert!(h.get("invalidation").is_some());
    }

    // Check a path-scoped provider (git)
    let git = providers.iter().find(|p| p["name"] == "git");
    if let Some(g) = git {
        assert_eq!(g["scope"].as_str(), Some("path"));
    }

    let verdicts = data.get("verdicts").and_then(|v| v.as_array()).expect("verdicts");
    assert!(!verdicts.is_empty());
    // PASS with count message should be present
    let has_count_pass = verdicts.iter().any(|v| {
        v["level"] == "PASS" && v["message"].as_str().unwrap_or("").contains("registered")
    });
    assert!(has_count_pass, "count PASS verdict missing: {verdicts:?}");

    handle.abort();
}

#[tokio::test]
async fn introspect_config_reports_path_and_parse_status() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "config"}))
        .await
        .unwrap();
    assert!(resp.ok, "error: {:?}", resp.error);
    let d = resp.data.unwrap();
    assert!(d.get("path").is_some(), "path key present (may be null)");
    assert!(d.get("parsed").and_then(|v| v.as_bool()).is_some());
    assert!(d.get("errors").and_then(|v| v.as_array()).is_some());
    assert!(
        d.get("provider_count_from_config").and_then(|v| v.as_u64()).is_some(),
        "provider_count_from_config missing or not a number"
    );
    let verdicts = d.get("verdicts").and_then(|v| v.as_array()).unwrap();
    assert!(!verdicts.is_empty());

    handle.abort();
}

#[tokio::test]
async fn introspect_cache_reports_totals_and_stale_ratio() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "cache"}))
        .await
        .unwrap();
    assert!(resp.ok, "error: {:?}", resp.error);
    let d = resp.data.unwrap();
    assert!(d.get("total_entries").and_then(|v| v.as_u64()).is_some());
    assert!(d.get("stale_entries").and_then(|v| v.as_u64()).is_some());
    assert!(d.get("stale_ratio").and_then(|v| v.as_f64()).is_some());
    let verdicts = d.get("verdicts").and_then(|v| v.as_array()).unwrap();
    assert!(!verdicts.is_empty());
    // PASS count message should be present
    let has_count = verdicts.iter().any(|v| {
        v["level"] == "PASS" && v["message"].as_str().unwrap_or("").contains("entries")
    });
    assert!(has_count, "count PASS verdict missing: {verdicts:?}");

    handle.abort();
}

#[tokio::test]
async fn introspect_backoff_returns_list_and_verdicts() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "backoff"}))
        .await
        .unwrap();
    assert!(resp.ok, "error: {:?}", resp.error);
    let d = resp.data.unwrap();
    assert!(d.get("backoff").and_then(|v| v.as_array()).is_some(), "backoff array missing");
    let verdicts = d.get("verdicts").and_then(|v| v.as_array()).expect("verdicts array");
    assert!(!verdicts.is_empty(), "at least one verdict expected");
    for v in verdicts {
        let level = v.get("level").and_then(|x| x.as_str()).expect("verdict has level");
        assert!(["PASS", "WARN", "FAIL"].contains(&level), "unexpected verdict level: {level}");
        assert!(v.get("message").and_then(|x| x.as_str()).is_some(), "verdict missing message");
    }

    handle.abort();
}

#[tokio::test]
async fn introspect_watches_returns_paths_and_verdicts() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "watches"}))
        .await
        .unwrap();
    assert!(resp.ok, "error: {:?}", resp.error);
    let d = resp.data.unwrap();
    assert!(d.get("paths").and_then(|v| v.as_array()).is_some(), "paths array missing");
    let verdicts = d.get("verdicts").and_then(|v| v.as_array()).expect("verdicts array");
    assert!(!verdicts.is_empty(), "at least one verdict expected");
    for v in verdicts {
        let level = v.get("level").and_then(|x| x.as_str()).expect("verdict has level");
        assert!(["PASS", "WARN", "FAIL"].contains(&level), "unexpected verdict level: {level}");
        assert!(v.get("message").and_then(|x| x.as_str()).is_some(), "verdict missing message");
    }

    handle.abort();
}

#[tokio::test]
async fn introspect_timers_returns_timers_and_verdicts() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "timers"}))
        .await
        .unwrap();
    assert!(resp.ok, "error: {:?}", resp.error);
    let d = resp.data.unwrap();
    assert!(d.get("timers").and_then(|v| v.as_array()).is_some(), "timers array missing");
    let verdicts = d.get("verdicts").and_then(|v| v.as_array()).expect("verdicts array");
    assert!(!verdicts.is_empty(), "at least one verdict expected");
    for v in verdicts {
        let level = v.get("level").and_then(|x| x.as_str()).expect("verdict has level");
        assert!(["PASS", "WARN", "FAIL"].contains(&level), "unexpected verdict level: {level}");
        assert!(v.get("message").and_then(|x| x.as_str()).is_some(), "verdict missing message");
    }

    handle.abort();
}

#[tokio::test]
async fn introspect_demand_returns_active_keys() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "demand"}))
        .await
        .unwrap();
    assert!(resp.ok, "error: {:?}", resp.error);
    let d = resp.data.unwrap();
    assert!(d.get("demand").and_then(|v| v.as_array()).is_some(), "demand array missing");
    let verdicts = d.get("verdicts").and_then(|v| v.as_array()).expect("verdicts array");
    assert!(!verdicts.is_empty(), "at least one verdict expected");
    for v in verdicts {
        let level = v.get("level").and_then(|x| x.as_str()).expect("verdict has level");
        assert!(["PASS", "WARN", "FAIL"].contains(&level), "unexpected verdict level: {level}");
        assert!(v.get("message").and_then(|x| x.as_str()).is_some(), "verdict missing message");
    }

    handle.abort();
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::test]
async fn introspect_procs_returns_sample_structure() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({
            "op": "introspect",
            "subject": "procs",
            "duration_secs": 1
        }))
        .await
        .expect("introspect procs request");

    // procs may fail in sandboxed environments; accept either ok or a recognisable error.
    if !resp.ok {
        let err = resp.error.as_deref().unwrap_or("");
        assert!(
            err.contains("procs")
                || err.contains("permission")
                || err.contains("eslogger")
                || err.contains("proc")
                || err.contains("snapshot"),
            "unexpected error: {err}"
        );
        handle.abort();
        return;
    }

    let d = resp.data.expect("payload present on success");
    assert!(
        d.get("duration_secs").and_then(|v| v.as_u64()).is_some(),
        "duration_secs missing or not a number"
    );
    assert!(
        d.get("samples").and_then(|v| v.as_array()).is_some(),
        "samples missing or not an array"
    );
    assert!(
        d.get("replacement_suggestions").and_then(|v| v.as_array()).is_some(),
        "replacement_suggestions missing or not an array"
    );
    assert!(
        d.get("verdicts").and_then(|v| v.as_array()).is_some(),
        "verdicts missing or not an array"
    );

    let verdicts = d.get("verdicts").and_then(|v| v.as_array()).unwrap();
    assert!(!verdicts.is_empty(), "at least one verdict expected");
    for v in verdicts {
        let level = v
            .get("level")
            .and_then(|x| x.as_str())
            .expect("verdict has level");
        assert!(
            ["INFO", "WARN", "PASS", "FAIL"].contains(&level),
            "unexpected verdict level: {level}"
        );
        assert!(
            v.get("message").and_then(|x| x.as_str()).is_some(),
            "verdict missing message"
        );
    }

    handle.abort();
}
