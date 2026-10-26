// Integration tests for the Request::Introspect per-subject payloads
// and the `comb check` CLI aggregation behaviour.
// Uses the in-process test daemon pattern from tests/suite/put_remove.rs.

use crate::common::daemon::{isolate, launch, stop};
use crate::common::poll::poll_until;
use beachcomber::boundaries::proc_snap::ProcessSnapshotter;
use beachcomber::client::Client;
use beachcomber::config::Config;
use beachcomber::proc_snapshot::{ProcSample, ProcSnapshotResult, ReplacementSuggestion};

async fn setup_daemon() -> (tempfile::TempDir, Client, tokio::task::JoinHandle<()>) {
    let tmp = tempfile::TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let config = Config::default();
    let (handle, ready) = beachcomber::daemon::start_in_process_polling(sock.clone(), config);
    ready.await.expect("daemon ready");
    let client = Client::new(sock);
    (tmp, client, handle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn introspect_daemon_returns_expected_fields() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .introspect("daemon", None)
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
    // config_path is absent when no file was loaded, as every absent field is.
    assert!(
        data.get("config_path").is_none_or(|v| v.is_string()),
        "config_path is a path or absent: {data}"
    );
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
async fn introspect_providers_lists_catalog_with_fields() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .introspect("providers", None)
        .expect("providers introspect");
    assert!(resp.ok, "error: {:?}", resp.error);
    let data = resp.data.expect("payload present");

    let providers = data
        .get("providers")
        .and_then(|v| v.as_array())
        .expect("providers array");
    assert!(!providers.is_empty(), "at least one provider registered");

    // Check a known provider like hostname
    let hostname = providers.iter().find(|p| p["name"] == "hostname");
    if let Some(h) = hostname {
        assert_eq!(h["source"].as_str(), Some("builtin"));
        // Providers expose per-source metadata under "sources"; check first source's fields.
        let sources = h.get("sources").and_then(|v| v.as_array());
        assert!(sources.is_some(), "hostname missing 'sources' array");
        if let Some(srcs) = sources {
            let first = srcs.first().expect("at least one source");
            assert!(first.get("fields").and_then(|v| v.as_array()).is_some());
        }
        assert!(h.get("invalidation").is_some());
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

    let resp = client.introspect("config", None).unwrap();
    assert!(resp.ok, "error: {:?}", resp.error);
    let d = resp.data.unwrap();
    assert!(d.get("path").is_some(), "path key present (may be null)");
    assert!(d.get("parsed").and_then(|v| v.as_bool()).is_some());
    assert!(d.get("errors").and_then(|v| v.as_array()).is_some());
    assert!(
        d.get("config_source_count")
            .and_then(|v| v.as_u64())
            .is_some(),
        "config_source_count missing or not a number"
    );
    let verdicts = d.get("verdicts").and_then(|v| v.as_array()).unwrap();
    assert!(!verdicts.is_empty());

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn introspect_cache_reports_totals_and_stale_ratio() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client.introspect("cache", None).unwrap();
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

    let resp = client.introspect("lifecycle", None).unwrap();
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
async fn introspect_watches_returns_watches_and_verdicts() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client.introspect("watches", None).unwrap();
    assert!(resp.ok, "error: {:?}", resp.error);
    let d = resp.data.unwrap();
    assert!(
        d.get("watches").and_then(|v| v.as_array()).is_some(),
        "watches array missing"
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

    let resp = client.introspect("timers", None).unwrap();
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

    let resp = client.introspect("demand", None).unwrap();
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

/// A process tracer that saw two `git` starts and a `zsh`, at once: what
/// `introspect procs` makes of a sample, without waiting out a window.
struct SawGit;

impl ProcessSnapshotter for SawGit {
    fn capture(&self, duration_secs: u64) -> Result<ProcSnapshotResult, String> {
        Ok(ProcSnapshotResult {
            duration_secs,
            total: 3,
            samples: vec![
                ProcSample {
                    command: "git".into(),
                    count: 2,
                    category: Some("git".into()),
                },
                ProcSample {
                    command: "zsh".into(),
                    count: 1,
                    category: Some("shell".into()),
                },
            ],
            replacement_suggestions: vec![ReplacementSuggestion {
                command_pattern: "git".into(),
                provider: "git".into(),
                field: "branch".into(),
            }],
        })
    }
}

/// An in-process daemon whose `introspect procs` samples through [`SawGit`].
async fn setup_sampling_daemon() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    tokio::task::JoinHandle<()>,
) {
    let tmp = tempfile::TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let (handle, ready) = beachcomber::daemon::start_in_process_polling_sampling(
        sock.clone(),
        Config::default(),
        std::sync::Arc::new(SawGit),
    );
    ready.await.expect("daemon ready");
    (tmp, sock, handle)
}

/// `introspect procs` hands the sampler the duration asked for and reports
/// each command, the replacements `comb get` offers, and a verdict on each.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn introspect_procs_returns_sample_structure() {
    let (_tmp, sock, handle) = setup_sampling_daemon().await;

    let resp = Client::new(sock)
        .introspect("procs", Some(7))
        .expect("introspect procs request");
    handle.abort();

    assert!(resp.ok, "error: {:?}", resp.error);
    let d = resp.data.expect("payload present on success");
    assert_eq!(
        d,
        serde_json::json!({
            "duration_secs": 7,
            "samples": [
                {"command": "git", "count": 2, "category": "git"},
                {"command": "zsh", "count": 1, "category": "shell"},
            ],
            "replacement_suggestions": [
                {"command_pattern": "git", "provider": "git", "field": "branch"},
            ],
            "verdicts": [
                {"level": "INFO", "message": "7s sample: 3 exec events"},
                {"level": "WARN", "message": "1 replacement opportunity: consider using `comb get` instead"},
            ],
        })
    );
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
            .introspect(subject, None)
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

    let output = isolate(
        &mut std::process::Command::new(&exe),
        tmp.path(),
        &tmp.path().join("cfg"),
        &tmp.path().join("no-daemon.sock"),
    )
    .args(["check", "--connect-timeout", "0", "daemon"])
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

/// `--connect-timeout` bounds the whole search: with nothing behind the socket
/// the check keeps retrying until the budget is spent, then exits 2. The flag
/// is placed after the subcommand here to cover its `global` placement.
#[test]
fn check_connect_timeout_bounds_the_search() {
    let tmp = tempfile::TempDir::new().unwrap();
    let exe = comb_binary();

    let start = std::time::Instant::now();
    let output = isolate(
        &mut std::process::Command::new(&exe),
        tmp.path(),
        &tmp.path().join("cfg"),
        &tmp.path().join("no-daemon.sock"),
    )
    .args(["check", "daemon", "--connect-timeout", "100ms"])
    .output()
    .expect("run comb check daemon");
    let elapsed = start.elapsed();

    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        elapsed >= std::time::Duration::from_millis(100),
        "should keep trying for the whole budget; elapsed={elapsed:?}"
    );
}

/// Daemon introspect payload contains at least one PASS verdict.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_introspect_has_pass_verdict() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .introspect("daemon", None)
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

/// The key introspect no longer carries.
const REMOVED_KEY: &str = "scope";

/// A Source's root is a question asked of a directory, not a property of the
/// provider: introspect describes neither a provider nor a Source as "global"
/// or "path".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn introspect_providers_has_no_scope() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .introspect("providers", None)
        .expect("providers introspect");
    assert!(resp.ok, "error: {:?}", resp.error);
    let data = resp.data.unwrap();
    let providers = data
        .get("providers")
        .and_then(|v| v.as_array())
        .expect("providers array");
    assert!(
        providers.iter().any(|p| p["name"] == "git"),
        "git is registered, so the Source level is checked too"
    );

    for p in providers {
        assert!(
            p.get(REMOVED_KEY).is_none(),
            "provider has a {REMOVED_KEY}: {p:?}"
        );
        for src in p
            .get("sources")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            assert!(
                src.get(REMOVED_KEY).is_none(),
                "source has a {REMOVED_KEY}: {src:?}"
            );
        }
    }

    handle.abort();
}

/// Providers introspect lists at least one provider with all required fields.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn providers_introspect_has_entries_with_required_fields() {
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .introspect("providers", None)
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
        // Fields are now per-source — check the "sources" array contains at least one source
        // with a fields entry. Static namespaces may have no sources (data-only).
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

    let resp = client.introspect("cache", None).expect("cache introspect");

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

    // The daemon writes managed defaults under `$XDG_CONFIG_HOME` on start.
    let mut cmd = std::process::Command::new(&exe);
    isolate(&mut cmd, tmp.path(), tmp.path(), &sock)
        .args(["daemon", "--exit-with-parent", "--socket"])
        .arg(&sock);
    let mut daemon = launch(&mut cmd);

    let output = isolate(
        &mut std::process::Command::new(&exe),
        tmp.path(),
        tmp.path(),
        &sock,
    )
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

    stop(&mut daemon);
}

// --- Reaper health surfacing (canon singleton.md invariant 13) ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn introspect_daemon_reaper_absent_when_not_attached() {
    // Embedded/test servers never attach reaper health; the field is
    // omitted, as every absent field is.
    let (_tmp, client, handle) = setup_daemon().await;

    let resp = client
        .introspect("daemon", None)
        .expect("request succeeded");
    assert!(resp.ok);
    let data = resp.data.expect("payload");
    assert!(
        data.get("reaper").is_none(),
        "reaper must be absent when health is not attached: {data}"
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

    let (handle, ready) = beachcomber::daemon::start_in_process_polling_with(
        sock.clone(),
        Config::default(),
        tokio_util::sync::CancellationToken::new(),
        Some(health),
    );
    ready.await.expect("daemon ready");
    let client = Client::new(sock);

    let resp = client
        .introspect("daemon", None)
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

    let (handle, ready) = beachcomber::daemon::start_in_process_polling_with(
        sock.clone(),
        Config::default(),
        tokio_util::sync::CancellationToken::new(),
        Some(health),
    );
    ready.await.expect("daemon ready");
    let client = Client::new(sock);

    let resp = client
        .introspect("daemon", None)
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

    let mut cmd = std::process::Command::new(&exe);
    isolate(&mut cmd, tmp.path(), tmp.path(), &sock)
        .args(["daemon", "--exit-with-parent", "--socket"])
        .arg(&sock);
    let mut daemon = launch(&mut cmd);

    let client = Client::new(sock.clone());
    let resp = client
        .introspect("daemon", None)
        .expect("introspect request");

    stop(&mut daemon);

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

    // A regression here means the daemon never exits, and the poll below fails
    // by panicking — kill it on the way out so no runaway process is left over.
    struct KillOnDrop(std::process::Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let exe = comb_binary();
    let mut daemon = KillOnDrop(
        isolate(
            &mut std::process::Command::new(&exe),
            tmp.path(),
            tmp.path(),
            &sock,
        )
        .args(["daemon", "--socket"])
        .arg(&sock)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn daemon"),
    );

    let exited = poll_until(
        "the `comb daemon` process after a failed bind",
        "the process to have exited rather than linger",
        std::time::Duration::from_secs(10),
        std::time::Duration::from_millis(20),
        || daemon.0.try_wait().expect("try_wait").is_some(),
        |exited| *exited,
    );
    assert!(exited, "daemon lingered past 10s after bind failure");
}

/// `comb check config` validates the config file client-side and says so: a
/// clean file gets a PASS line tagged `local`, so a reader can tell a valid
/// config from a check that never ran.
#[test]
fn check_config_reports_its_own_local_verdict() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg_dir = tmp.path().join("beachcomber");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("config.toml"),
        "[daemon]\nlog_level = \"info\"\n",
    )
    .unwrap();

    let output = isolate(
        &mut std::process::Command::new(comb_binary()),
        tmp.path(),
        tmp.path(),
        &tmp.path().join("no-daemon.sock"),
    )
    .args(["check", "--connect-timeout", "0", "config"])
    .output()
    .expect("run comb check config");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[PASS] local"),
        "expected a local PASS line, got:\n{stdout}"
    );
}

/// `comb check config` judges the configuration with the variables a daemon
/// started on it would have, the env file's among them: a `~` in the plugin
/// path is the env file's `HOME` when the checking shell has none.
#[test]
fn check_config_sees_the_env_files_variables() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg_dir = tmp.path().join("beachcomber");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("config.toml"),
        "[daemon.plugin]\npath = [\"~/plugins\"]\n",
    )
    .unwrap();
    std::fs::write(
        cfg_dir.join("env"),
        format!("HOME={}\n", tmp.path().display()),
    )
    .unwrap();

    let output = isolate(
        &mut std::process::Command::new(comb_binary()),
        tmp.path(),
        tmp.path(),
        &tmp.path().join("no-daemon.sock"),
    )
    .env_remove("HOME")
    .args(["check", "--connect-timeout", "0", "config"])
    .output()
    .expect("run comb check config");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[PASS] local") && !stdout.contains("HOME is not set"),
        "expected the env file's HOME to stand, got:\n{stdout}"
    );
}

/// `sys` is the compiled-in plugin's name: a `libsys` on `[daemon.plugin]
/// path` is never loaded, and `comb check config` warns of it, naming it.
#[test]
fn a_libsys_on_the_plugin_path_is_a_warning() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg_dir = tmp.path().join("beachcomber");
    let plugins = tmp.path().join("plugins");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::create_dir_all(&plugins).unwrap();
    let ext = if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    };
    let shadow = plugins.join(format!("libsys.{ext}"));
    std::fs::write(&shadow, b"").unwrap();
    std::fs::write(
        cfg_dir.join("config.toml"),
        format!(
            "[daemon.plugin]\npath = [\"{}\"]\n\n\
             [daemon.providers.box.sources.host]\n\
             data = \"{{{{ plugin('sys', 'hostname') }}}}\"\n\
             result = \"{{{{ data }}}}\"\n",
            plugins.display()
        ),
    )
    .unwrap();

    let output = isolate(
        &mut std::process::Command::new(comb_binary()),
        tmp.path(),
        tmp.path(),
        &tmp.path().join("no-daemon.sock"),
    )
    .args(["check", "--connect-timeout", "0", "config"])
    .output()
    .expect("run comb check config");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[WARN] local")
            && stdout.contains(&format!(
                "[daemon.plugin] path: {} is shadowed by the built-in sys plugin",
                shadow.display()
            ))
            && !stdout.contains("[FAIL] local"),
        "expected a local WARN naming the shadowed library, got:\n{stdout}"
    );
}

/// A client template reading a name no provider has is a warning in
/// `comb check config`, not a failure: a `put` may yet make the provider.
#[test]
fn check_config_warns_of_a_template_reading_no_provider() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg_dir = tmp.path().join("beachcomber");
    std::fs::create_dir_all(cfg_dir.join("defaults")).unwrap();
    std::fs::copy(
        concat!(env!("CARGO_MANIFEST_DIR"), "/defaults/00-comb.toml"),
        cfg_dir.join("defaults/00-comb.toml"),
    )
    .unwrap();
    std::fs::write(
        cfg_dir.join("config.toml"),
        "[client.providers.p]\ntemplate.t = \"{{ gti.branch }}\"\n",
    )
    .unwrap();

    let output = isolate(
        &mut std::process::Command::new(comb_binary()),
        tmp.path(),
        tmp.path(),
        &tmp.path().join("no-daemon.sock"),
    )
    .args(["check", "--connect-timeout", "0", "config"])
    .output()
    .expect("run comb check config");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[WARN] local    config(")
            && stdout.contains("[client.providers.p] template.t: unknown name 'gti'")
            && !stdout.contains("[FAIL] local"),
        "expected a local WARN naming the template, got:\n{stdout}"
    );
}

/// With no `config.toml`, `check` and `check config` say that and only that: the
/// `defaults/` and `conf.d/` files may still be composed, so it does not claim
/// comb is running on its built-in values. A bare `check` runs every subject,
/// `procs` at the daemon's default sample.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn check_without_a_config_toml_says_so() {
    let (tmp, sock, handle) = setup_sampling_daemon().await;
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let outputs = tokio::task::spawn_blocking(move || {
        [&["check"][..], &["check", "config"]].map(|args| {
            let out = isolate(
                &mut std::process::Command::new(comb_binary()),
                &home,
                &home.join(".config"),
                &sock,
            )
            .current_dir("/")
            .args(args)
            .output()
            .unwrap();
            (args, String::from_utf8_lossy(&out.stdout).into_owned())
        })
    })
    .await
    .unwrap();
    handle.abort();

    for (args, stdout) in &outputs {
        assert!(
            stdout.contains("no config.toml") && !stdout.contains("using defaults"),
            "comb {args:?}:\n{stdout}"
        );
    }
    let (_, bare) = &outputs[0];
    for header in [
        "Daemon\n",
        "Config\n",
        "Providers",
        "Procs (2s sample — 3 exec events)\n",
    ] {
        assert!(bare.contains(header), "comb check: no {header:?}:\n{bare}");
    }
}

/// The drop-in scanline generates, as phase 6 writes it (the fixture the
/// scanline cells are checked against, `libbeachcomber/tests/suite/scanline.rs`),
/// checks clean: the configuration is valid and no finding names the file.
#[test]
fn the_scanline_dropin_checks_clean() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg_dir = tmp.path().join("beachcomber");
    std::fs::create_dir_all(cfg_dir.join("conf.d")).unwrap();
    std::fs::write(
        cfg_dir.join("config.toml"),
        "[daemon]\nlog_level = \"info\"\n",
    )
    .unwrap();
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/libbeachcomber/tests/fixtures/scanline/10-scanline.toml"
        ),
        cfg_dir.join("conf.d/10-scanline.toml"),
    )
    .unwrap();

    let output = isolate(
        &mut std::process::Command::new(comb_binary()),
        tmp.path(),
        tmp.path(),
        &tmp.path().join("no-daemon.sock"),
    )
    .args(["check", "--connect-timeout", "0", "config"])
    .output()
    .expect("run comb check config");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[PASS] local    config file valid")
            && !stdout.contains("[FAIL] local")
            && !stdout.contains("10-scanline.toml"),
        "expected the scanline drop-in to check clean, got:\n{stdout}"
    );
}

/// `comb check config` on a file still carrying the removed `[providers]` root
/// prints a local FAIL line that says where its contents went.
#[test]
fn check_config_fails_a_providers_root_naming_where_it_went() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg_dir = tmp.path().join("beachcomber");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("config.toml"),
        "[providers.conda]\nvirtual.env = \"{{ env.CONDA_DEFAULT_ENV }}\"\n",
    )
    .unwrap();

    let output = isolate(
        &mut std::process::Command::new(comb_binary()),
        tmp.path(),
        tmp.path(),
        &tmp.path().join("no-daemon.sock"),
    )
    .args(["check", "--connect-timeout", "0", "config"])
    .output()
    .expect("run comb check config");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[FAIL] local    config(")
            && stdout.contains("unknown table [providers]: a provider's settings are")
            && stdout.contains("templates live in [client.providers.<p>] template.<leaf>"),
        "expected a local FAIL line naming where [providers] went, got:\n{stdout}"
    );
}

/// The same config refuses the daemon, and the refusal is an exit 2 — not the
/// 0 of a clean stop, which a supervisor could not tell apart. It never binds.
#[test]
fn comb_daemon_refused_by_config_validation_exits_two_and_binds_nothing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg_dir = tmp.path().join("beachcomber");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("config.toml"),
        "[providers.conda]\nvirtual.env = \"{{ env.CONDA_DEFAULT_ENV }}\"\n",
    )
    .unwrap();
    let sock = tmp.path().join("sock");

    // `.output()` waits for the process to exit: a refused daemon returns
    // on its own, so a hang here is the bug.
    let output = isolate(
        &mut std::process::Command::new(comb_binary()),
        tmp.path(),
        tmp.path(),
        &sock,
    )
    .args(["daemon", "--exit-with-parent", "--socket"])
    .arg(&sock)
    .output()
    .expect("run comb daemon");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "stderr:\n{stderr}");
    assert!(
        stderr.contains("error: daemon: refusing to start: the configuration has "),
        "the refusal is logged, got:\n{stderr}"
    );
    assert!(!sock.exists(), "it never bound its socket");
}
