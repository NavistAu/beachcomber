// Integration tests for the Request::Introspect per-subject payloads
// and the `comb check` CLI aggregation behaviour.
// Uses the in-process test daemon pattern from tests/put_null.rs.

use beachcomber::client::Client;
use beachcomber::config::Config;

async fn setup_daemon() -> (tempfile::TempDir, Client, tokio::task::JoinHandle<()>) {
    let tmp = tempfile::TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let config = Config::default();
    let handle = beachcomber::daemon::start_in_process(sock.clone(), config);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let client = Client::new(sock);
    (tmp, client, handle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn introspect_daemon_returns_expected_fields() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({
            "op": "introspect",
            "subject": "daemon"
        }))
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn introspect_providers_lists_catalog_with_scope_and_fields() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "providers"}))
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
        // Providers expose per-source metadata under "sources"; check first source's fields.
        let sources = h.get("sources").and_then(|v| v.as_array());
        assert!(sources.is_some(), "hostname missing 'sources' array");
        if let Some(srcs) = sources {
            let first = srcs.first().expect("at least one source");
            assert!(first.get("fields").and_then(|v| v.as_array()).is_some());
        }
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn introspect_config_reports_path_and_parse_status() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "config"}))
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn introspect_cache_reports_totals_and_stale_ratio() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "cache"}))
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn introspect_lifecycle_returns_list_and_verdicts() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "lifecycle"}))
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn introspect_watches_returns_paths_and_verdicts() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "watches"}))
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn introspect_timers_returns_timers_and_verdicts() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "timers"}))
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn introspect_demand_returns_active_keys() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "demand"}))
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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn introspect_procs_returns_sample_structure() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({
            "op": "introspect",
            "subject": "procs",
            "duration_secs": 1
        }))
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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

/// Locate the `comb` binary for real-process e2e tests. Test binaries live in
/// `target/debug/deps/`; the bin is one level up. Asserts existence — a silent
/// skip here previously masked these tests never running under nextest.
fn comb_binary() -> std::path::PathBuf {
    let mut dir = std::env::current_exe()
        .expect("current_exe")
        .parent()
        .expect("parent dir")
        .to_path_buf();
    if dir.ends_with("deps") {
        dir.pop();
    }
    let exe = dir.join("comb");
    assert!(
        exe.exists(),
        "comb binary not found at {} — build layout changed?",
        exe.display()
    );
    exe
}

/// `comb check daemon` with an unreachable socket exits 2 and prints a FAIL line.
/// Redirect via BEACHCOMBER_SOCKET so `resolve_socket_path` finds a path with no daemon.
#[test]
fn check_daemon_unreachable_exits_two() {
    let tmp = tempfile::TempDir::new().unwrap();

    let exe = comb_binary();

    let output = std::process::Command::new(&exe)
        // Point BEACHCOMBER_SOCKET at a temp path with no daemon behind it.
        .env("BEACHCOMBER_SOCKET", tmp.path().join("no-daemon.sock"))
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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_introspect_has_pass_verdict() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "daemon"}))
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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn providers_introspect_has_entries_with_required_fields() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "providers"}))
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
        // Fields are now per-source — check the "sources" array contains at least one source
        // with a fields entry. Virtual providers may have no sources (data-only).
        if let Some(srcs) = p.get("sources").and_then(|v| v.as_array())
            && !srcs.is_empty()
        {
            assert!(
                srcs[0].get("fields").is_some(),
                "source missing fields: {p:?}"
            );
        }
        assert!(
            p.get("invalidation").is_some(),
            "provider missing invalidation: {p:?}"
        );
    }

    handle.abort();
}

/// Cache stale_ratio is in the valid range [0.0, 1.0].
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cache_introspect_stale_ratio_coherent() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "cache"}))
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

/// Canon singleton.md invariant 12: the chosen watch backend is observable via
/// `comb check daemon` — a PASS line for native, a WARN line when degraded.
#[test]
fn check_daemon_reports_watch_backend() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sock = tmp.path().join("beachcomber").join("sock");

    let exe = comb_binary();

    let mut daemon = std::process::Command::new(&exe)
        .args(["daemon", "--exit-with-parent", "--socket"])
        .arg(&sock)
        .spawn()
        .expect("spawn daemon");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !sock.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(sock.exists(), "daemon never bound socket");

    let output = std::process::Command::new(&exe)
        .env("BEACHCOMBER_SOCKET", &sock)
        .args(["check", "daemon"])
        .output()
        .expect("run comb check daemon");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("watch backend:"),
        "check daemon must report the watch backend, got:\n{stdout}"
    );
    assert!(
        stdout.contains("watch backend: native fs events")
            || stdout.contains("watch backend: polling"),
        "backend must be native or polling, got:\n{stdout}"
    );

    let _ = daemon.kill();
    let _ = daemon.wait();
}

// --- Reaper health surfacing (canon singleton.md invariant 13) ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn introspect_daemon_reaper_null_when_not_attached() {
    // Embedded/test servers never attach reaper health; the field is null.
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "daemon"}))
        .expect("request succeeded");
    assert!(resp.ok);
    let data = resp.data.expect("payload");
    assert!(
        data.get("reaper").expect("reaper key present").is_null(),
        "reaper must be null when health is not attached"
    );

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn introspect_daemon_reaper_confined_surfaces_warn_verdict() {
    use std::sync::atomic::Ordering::Relaxed;

    let tmp = tempfile::TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let health = std::sync::Arc::new(beachcomber::singleton::ReaperHealth::default());
    health.armed.store(true, Relaxed);
    health.visibility_ok.store(false, Relaxed); // confined
    health.kill_denied_total.store(2, Relaxed);

    let handle = beachcomber::daemon::start_in_process_with_reaper(
        sock.clone(),
        Config::default(),
        tokio_util::sync::CancellationToken::new(),
        Some(health),
    );
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let client = Client::new(sock);

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "daemon"}))
        .expect("request succeeded");
    assert!(resp.ok);
    let data = resp.data.expect("payload");

    let reaper = data.get("reaper").expect("reaper present");
    assert_eq!(
        reaper.get("visibility").and_then(|v| v.as_str()),
        Some("confined")
    );
    assert_eq!(reaper.get("armed").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(reaper.get("kill_denied").and_then(|v| v.as_u64()), Some(2));

    let verdicts = data
        .get("verdicts")
        .and_then(|v| v.as_array())
        .expect("verdicts");
    assert!(
        verdicts.iter().any(|v| {
            v.get("level").and_then(|l| l.as_str()) == Some("WARN")
                && v.get("message")
                    .and_then(|m| m.as_str())
                    .is_some_and(|m| m.contains("reaper visibility degraded"))
        }),
        "confined reaper must produce a WARN verdict, got: {verdicts:?}"
    );
    assert!(
        verdicts.iter().any(|v| {
            v.get("level").and_then(|l| l.as_str()) == Some("WARN")
                && v.get("message")
                    .and_then(|m| m.as_str())
                    .is_some_and(|m| m.contains("denied by the OS"))
        }),
        "kill_denied > 0 must produce a WARN verdict, got: {verdicts:?}"
    );

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn introspect_daemon_reaper_healthy_surfaces_pass_verdict() {
    use std::sync::atomic::Ordering::Relaxed;

    let tmp = tempfile::TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let health = std::sync::Arc::new(beachcomber::singleton::ReaperHealth::default());
    health.armed.store(true, Relaxed);
    health.visibility_ok.store(true, Relaxed);

    let handle = beachcomber::daemon::start_in_process_with_reaper(
        sock.clone(),
        Config::default(),
        tokio_util::sync::CancellationToken::new(),
        Some(health),
    );
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let client = Client::new(sock);

    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "daemon"}))
        .expect("request succeeded");
    assert!(resp.ok);
    let data = resp.data.expect("payload");

    let reaper = data.get("reaper").expect("reaper present");
    assert_eq!(
        reaper.get("visibility").and_then(|v| v.as_str()),
        Some("system-wide")
    );
    let verdicts = data
        .get("verdicts")
        .and_then(|v| v.as_array())
        .expect("verdicts");
    assert!(
        verdicts.iter().any(|v| {
            v.get("level").and_then(|l| l.as_str()) == Some("PASS")
                && v.get("message")
                    .and_then(|m| m.as_str())
                    .is_some_and(|m| m.contains("system-wide process visibility"))
        }),
        "healthy reaper must produce a PASS verdict, got: {verdicts:?}"
    );

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn env_override_daemon_does_not_arm_reaper() {
    // Canon singleton.md §"Who reaps": reaper resolution ignores
    // $BEACHCOMBER_SOCKET, so a daemon bound via the env override is a side
    // daemon — introspect must report reaper.armed == false. This is the
    // fratricide guard: two daemons of one uid never both hold the reaper role.
    let tmp = tempfile::TempDir::new().unwrap();
    let sock = tmp.path().join("override.sock");

    let exe = comb_binary();

    let mut daemon = std::process::Command::new(&exe)
        .env("BEACHCOMBER_SOCKET", &sock)
        .args(["daemon", "--exit-with-parent", "--socket"])
        .arg(&sock)
        .spawn()
        .expect("spawn daemon");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !sock.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(sock.exists(), "daemon never bound socket");

    let client = Client::new(sock.clone());
    let resp = client
        .send_raw(serde_json::json!({"op": "introspect", "subject": "daemon"}))
        .expect("introspect request");

    let _ = daemon.kill();
    let _ = daemon.wait();

    assert!(resp.ok, "introspect failed: {:?}", resp.error);
    let data = resp.data.expect("payload");
    let armed = data.pointer("/reaper/armed").and_then(|v| v.as_bool());
    assert_eq!(
        armed,
        Some(false),
        "env-override daemon must not arm the reaper; payload: {data}"
    );
}

/// Regression: a daemon whose bind fails (here: socket path over SUN_LEN) must
/// EXIT, not linger. Previously `scheduler_task.await` blocked forever after a
/// server error and the process ignored SIGTERM (roadmap 2026-07-23 finding).
#[test]
fn daemon_exits_when_bind_fails() {
    let tmp = tempfile::TempDir::new().unwrap();
    // Build a path comfortably past the 104-byte unix-socket limit.
    let long = "x".repeat(120);
    let sock = tmp.path().join(long).join("sock");

    let exe = comb_binary();
    let mut daemon = std::process::Command::new(&exe)
        .args(["daemon", "--socket"])
        .arg(&sock)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn daemon");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match daemon.try_wait().expect("try_wait") {
            Some(_) => break, // exited — the regression is fixed
            None if std::time::Instant::now() > deadline => {
                let _ = daemon.kill();
                let _ = daemon.wait();
                panic!("daemon lingered past 10s after bind failure");
            }
            None => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }
}
