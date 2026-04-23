// Integration tests for the Request::Introspect per-subject payloads
// and the `comb check` CLI aggregation behaviour.
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
        data.get("requests_total")
            .and_then(|v| v.as_u64())
            .is_some(),
        "requests_total missing or not a number"
    );
    assert!(
        data.get("in_flight").and_then(|v| v.as_u64()).is_some(),
        "in_flight missing or not a number"
    );
    assert!(
        data.get("active_watchers")
            .and_then(|v| v.as_u64())
            .is_some(),
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

    let providers = data
        .get("providers")
        .and_then(|v| v.as_array())
        .expect("providers array");
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

    let verdicts = data
        .get("verdicts")
        .and_then(|v| v.as_array())
        .expect("verdicts");
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
        d.get("provider_count_from_config")
            .and_then(|v| v.as_u64())
            .is_some(),
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
    let has_count = verdicts
        .iter()
        .any(|v| v["level"] == "PASS" && v["message"].as_str().unwrap_or("").contains("entries"));
    assert!(has_count, "count PASS verdict missing: {verdicts:?}");

    handle.abort();
}

#[tokio::test]
async fn introspect_lifecycle_returns_list_and_verdicts() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "lifecycle"}))
        .await
        .unwrap();
    assert!(resp.ok, "error: {:?}", resp.error);
    let d = resp.data.unwrap();
    assert!(
        d.get("lifecycle").and_then(|v| v.as_array()).is_some(),
        "lifecycle array missing"
    );
    let verdicts = d
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
async fn introspect_watches_returns_paths_and_verdicts() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "watches"}))
        .await
        .unwrap();
    assert!(resp.ok, "error: {:?}", resp.error);
    let d = resp.data.unwrap();
    assert!(
        d.get("paths").and_then(|v| v.as_array()).is_some(),
        "paths array missing"
    );
    let verdicts = d
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
async fn introspect_timers_returns_timers_and_verdicts() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "timers"}))
        .await
        .unwrap();
    assert!(resp.ok, "error: {:?}", resp.error);
    let d = resp.data.unwrap();
    assert!(
        d.get("timers").and_then(|v| v.as_array()).is_some(),
        "timers array missing"
    );
    let verdicts = d
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
async fn introspect_demand_returns_active_keys() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "demand"}))
        .await
        .unwrap();
    assert!(resp.ok, "error: {:?}", resp.error);
    let d = resp.data.unwrap();
    assert!(
        d.get("demand").and_then(|v| v.as_array()).is_some(),
        "demand array missing"
    );
    let verdicts = d
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
        d.get("replacement_suggestions")
            .and_then(|v| v.as_array())
            .is_some(),
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

// ── CLI aggregation tests ──────────────────────────────────────────────────

/// All eight fast subjects (excluding procs which requires elevated permissions)
/// can be queried via the introspect op without error, and each returns verdicts.
#[tokio::test]
async fn all_subjects_reachable_via_introspect() {
    let (_tmp, client, handle) = setup_daemon().await;

    let subjects = [
        "daemon",
        "config",
        "providers",
        "cache",
        "watches",
        "lifecycle",
        "timers",
        "demand",
    ];

    for subject in subjects {
        let resp = client
            .send_raw(serde_json::json!({"op": "introspect", "subject": subject}))
            .await
            .unwrap_or_else(|e| panic!("introspect {subject} failed: {e}"));
        assert!(
            resp.ok,
            "subject={subject} returned error: {:?}",
            resp.error
        );
        let data = resp.data.expect("payload present");
        let verdicts = data
            .get("verdicts")
            .and_then(|v| v.as_array())
            .expect("verdicts array present");
        assert!(!verdicts.is_empty(), "subject={subject} has no verdicts");
    }

    handle.abort();
}

/// `comb check daemon` with an unreachable socket exits 2 and prints a FAIL line.
/// Redirect via XDG_RUNTIME_DIR so `resolve_socket_path` finds a path with no daemon.
#[test]
fn check_daemon_unreachable_exits_two() {
    let tmp = tempfile::TempDir::new().unwrap();

    // Find the `comb` binary next to the test runner.
    let exe = std::env::current_exe()
        .expect("current_exe")
        .parent()
        .expect("parent dir")
        .join("comb");

    if !exe.exists() {
        // Binary not built yet (e.g. running with `cargo test --no-run`). Skip.
        return;
    }

    let output = std::process::Command::new(&exe)
        // Point XDG_RUNTIME_DIR at a temp dir with no daemon socket inside it.
        .env("XDG_RUNTIME_DIR", tmp.path())
        .args(["check", "daemon"])
        .output()
        .expect("run comb check daemon");

    assert_eq!(
        output.status.code(),
        Some(2),
        "expected exit 2, got: {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[FAIL]"),
        "expected [FAIL] in output, got: {stdout}"
    );
}

/// Daemon introspect payload contains at least one PASS verdict.
#[tokio::test]
async fn daemon_introspect_has_pass_verdict() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "daemon"}))
        .await
        .expect("daemon introspect");

    assert!(resp.ok, "error: {:?}", resp.error);
    let data = resp.data.unwrap();
    let verdicts = data
        .get("verdicts")
        .and_then(|v| v.as_array())
        .expect("verdicts");

    let has_pass = verdicts
        .iter()
        .any(|v| v.get("level").and_then(|l| l.as_str()) == Some("PASS"));
    assert!(has_pass, "expected at least one PASS verdict: {verdicts:?}");

    handle.abort();
}

/// Providers introspect lists at least one provider with all required fields.
#[tokio::test]
async fn providers_introspect_has_entries_with_required_fields() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "providers"}))
        .await
        .expect("providers introspect");

    assert!(resp.ok, "error: {:?}", resp.error);
    let data = resp.data.unwrap();
    let providers = data
        .get("providers")
        .and_then(|v| v.as_array())
        .expect("providers array");
    assert!(!providers.is_empty(), "expected at least one provider");

    for p in providers {
        assert!(p.get("name").is_some(), "provider missing name: {p:?}");
        assert!(p.get("source").is_some(), "provider missing source: {p:?}");
        assert!(p.get("scope").is_some(), "provider missing scope: {p:?}");
        assert!(p.get("fields").is_some(), "provider missing fields: {p:?}");
        assert!(
            p.get("invalidation").is_some(),
            "provider missing invalidation: {p:?}"
        );
    }

    handle.abort();
}

/// Cache stale_ratio is in the valid range [0.0, 1.0].
#[tokio::test]
async fn cache_introspect_stale_ratio_coherent() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "cache"}))
        .await
        .expect("cache introspect");

    assert!(resp.ok, "error: {:?}", resp.error);
    let data = resp.data.unwrap();
    let ratio = data
        .get("stale_ratio")
        .and_then(|v| v.as_f64())
        .expect("stale_ratio present");
    assert!(
        (0.0..=1.0).contains(&ratio),
        "stale_ratio out of range: {ratio}"
    );

    handle.abort();
}
