//! Integration tests for the daemon singleton property and socket-path resolution.

use beachcomber::config::Config;
use std::path::PathBuf;

#[test]
fn resolve_socket_path_prefers_explicit_override() {
    let mut cfg = Config::default();
    cfg.daemon.socket_path = Some("/custom/path".into());
    assert_eq!(cfg.resolve_socket_path(), PathBuf::from("/custom/path"));
}

#[test]
fn resolve_socket_path_falls_back_to_tmp_not_tmpdir() {
    let cfg = Config::default();
    // Remove XDG_RUNTIME_DIR and set TMPDIR to a custom value that MUST be ignored.
    // temp_env restores both vars to their original values via Drop.
    let path = temp_env::with_vars(
        [
            ("XDG_RUNTIME_DIR", None::<&str>),
            ("TMPDIR", Some("/per/shell/tmpdir")),
        ],
        || cfg.resolve_socket_path(),
    );

    let path_str = path.to_string_lossy();
    assert!(
        path_str.starts_with("/tmp/beachcomber-"),
        "expected /tmp/beachcomber-* prefix, got {path_str}"
    );
    assert!(
        path_str.ends_with("/sock"),
        "expected /sock suffix, got {path_str}"
    );
}

#[test]
fn resolve_socket_path_ignores_session_scoped_env() {
    let cfg = Config::default();
    // temp_env serializes concurrent env mutations via a process-wide mutex and
    // restores both vars to their original values via Drop.
    let path = temp_env::with_vars(
        [
            ("XDG_RUNTIME_DIR", Some("/per/session/runtime")),
            ("TMPDIR", Some("/per/shell/tmpdir")),
        ],
        || cfg.resolve_socket_path(),
    );

    let path_str = path.to_string_lossy();
    assert!(
        path_str.starts_with("/tmp/beachcomber-"),
        "expected /tmp/beachcomber-* prefix, got {path_str}"
    );
    assert!(
        path_str.ends_with("/sock"),
        "expected /sock suffix, got {path_str}"
    );
    assert!(
        !path_str.contains("/per/shell/tmpdir"),
        "TMPDIR must not influence resolution, got {path_str}"
    );
    assert!(
        !path_str.contains("/per/session/runtime"),
        "XDG_RUNTIME_DIR must not influence resolution, got {path_str}"
    );
}

use beachcomber::singleton::{
    PidFileRecord, SingletonLock, SingletonLockError, SupersessionDecision, decide_supersession,
};

const TEST_HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TEST_HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[test]
fn singleton_lock_creates_pid_file_and_holds_flock() {
    let tmpdir = tempfile::tempdir().unwrap();
    let pid_path = tmpdir.path().join("pid");

    let lock = SingletonLock::acquire(&pid_path, "0.5.1+sha.abc", TEST_HASH_A)
        .expect("first acquire succeeds");
    assert!(pid_path.exists(), "pid file should be created");

    let contents = std::fs::read_to_string(&pid_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
    assert_eq!(parsed["pid"].as_u64().unwrap(), std::process::id() as u64);
    assert_eq!(parsed["version"].as_str().unwrap(), "0.5.1+sha.abc");
    assert_eq!(parsed["binary_hash"].as_str().unwrap(), TEST_HASH_A);
    assert!(parsed["binary"].is_string());
    assert!(parsed["started_unix_ms"].is_number());

    drop(lock);
    assert!(!pid_path.exists(), "pid file should be deleted on drop");
}

#[test]
fn singleton_lock_contested_by_same_process_fails() {
    let tmpdir = tempfile::tempdir().unwrap();
    let pid_path = tmpdir.path().join("pid");

    let _first = SingletonLock::acquire(&pid_path, "0.5.1+sha.abc", TEST_HASH_A).unwrap();
    let second = SingletonLock::acquire(&pid_path, "0.5.1+sha.abc", TEST_HASH_A);
    assert!(matches!(
        second,
        Err(SingletonLockError::AlreadyHeld { .. })
    ));
}

#[test]
fn supersession_same_binary_hash_serving_means_no_op() {
    let existing = PidFileRecord {
        pid: 12345,
        version: "0.5.1+sha.abc".into(),
        binary: "/path/to/comb".into(),
        binary_hash: TEST_HASH_A.into(),
        started_unix_ms: 0,
    };
    // Same build AND the owner is serving its socket → leave it alone.
    let decision = decide_supersession(&existing, TEST_HASH_A, true);
    assert!(matches!(decision, SupersessionDecision::ExitSilent));
}

#[test]
fn supersession_same_binary_hash_not_serving_supersedes() {
    // Same build but the owner is NOT serving (wedged between flock and bind,
    // or its socket was deleted) → supersede so a healthy daemon rebinds.
    let existing = PidFileRecord {
        pid: 12345,
        version: "0.5.1+sha.abc".into(),
        binary: "/path/to/comb".into(),
        binary_hash: TEST_HASH_A.into(),
        started_unix_ms: 0,
    };
    let decision = decide_supersession(&existing, TEST_HASH_A, false);
    match decision {
        SupersessionDecision::Supersede { existing_pid } => assert_eq!(existing_pid, 12345),
        _ => panic!("expected Supersede, got {decision:?}"),
    }
}

#[test]
fn supersession_different_binary_hash_means_supersede() {
    let existing = PidFileRecord {
        pid: 12345,
        version: "0.5.0".into(),
        binary: "/path/to/comb".into(),
        binary_hash: TEST_HASH_A.into(),
        started_unix_ms: 0,
    };
    // Different build supersedes regardless of serving state.
    let decision = decide_supersession(&existing, TEST_HASH_B, true);
    match decision {
        SupersessionDecision::Supersede { existing_pid } => assert_eq!(existing_pid, 12345),
        _ => panic!("expected Supersede, got {decision:?}"),
    }
}

#[test]
fn supersession_same_version_different_hash_still_supersedes() {
    // Two dev builds at the same cargo version but different binaries —
    // human version matches, but binary_hash differs, so we supersede.
    let existing = PidFileRecord {
        pid: 12345,
        version: "0.5.1".into(),
        binary: "/path/to/comb".into(),
        binary_hash: TEST_HASH_A.into(),
        started_unix_ms: 0,
    };
    let decision = decide_supersession(&existing, TEST_HASH_B, true);
    assert!(matches!(decision, SupersessionDecision::Supersede { .. }));
}

#[test]
fn supersede_existing_kills_target_process() {
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep");
    let pid = child.id();

    // Call supersede_existing. It may report "survived SIGKILL" because we're the parent
    // of the target and the process becomes a zombie until we wait() — that's a test-env
    // artefact, not a production bug.
    let _ = beachcomber::singleton::supersede_existing(pid, std::time::Duration::from_millis(500));

    // Reap the zombie.
    let _ = child.wait();

    // After reaping, the kernel frees the PID slot.
    std::thread::sleep(std::time::Duration::from_millis(50));
    let still_alive = unsafe { libc::kill(pid as libc::pid_t, 0) };
    assert!(
        still_alive != 0,
        "process should be gone; kill(0) returned {still_alive}"
    );
}

#[test]
fn supersede_existing_refuses_pid_one() {
    let result =
        beachcomber::singleton::supersede_existing(1, std::time::Duration::from_millis(100));
    assert!(result.is_err(), "expected error refusing pid 1");
}

#[test]
fn supersede_existing_refuses_self() {
    let me = std::process::id();
    let result =
        beachcomber::singleton::supersede_existing(me, std::time::Duration::from_millis(100));
    assert!(result.is_err(), "expected error refusing self pid");
}

#[test]
fn binary_newer_than_returns_true_when_file_modified_after_start() {
    let tmpdir = tempfile::tempdir().unwrap();
    let fake_binary = tmpdir.path().join("fake");
    std::fs::write(&fake_binary, b"hello").unwrap();

    let process_start_ms = 0u64; // 1970-01-01 — far before fake_binary's mtime
    let result = beachcomber::singleton::binary_newer_than(&fake_binary, process_start_ms)
        .expect("metadata read");
    assert!(result, "freshly written file should be newer than 1970");
}

#[test]
fn binary_newer_than_returns_false_when_file_older_than_start() {
    let tmpdir = tempfile::tempdir().unwrap();
    let fake_binary = tmpdir.path().join("fake");
    std::fs::write(&fake_binary, b"hello").unwrap();

    // Use a far-future timestamp so the file's mtime is older.
    let process_start_ms = u64::MAX / 2;
    let result = beachcomber::singleton::binary_newer_than(&fake_binary, process_start_ms)
        .expect("metadata read");
    assert!(!result, "file should be older than far-future timestamp");
}

use beachcomber::boundaries::socket::SocketProbe;
use std::path::Path;
use std::time::Duration;

struct AlwaysServing;
impl SocketProbe for AlwaysServing {
    fn is_serving(&self, _socket: &Path) -> bool {
        true
    }
}

struct NeverServing;
impl SocketProbe for NeverServing {
    fn is_serving(&self, _socket: &Path) -> bool {
        false
    }
}

#[test]
fn acquire_or_supersede_same_hash_serving_exits_silent() {
    let tmpdir = tempfile::tempdir().unwrap();
    let pid_path = tmpdir.path().join("pid");
    let socket_path = tmpdir.path().join("sock");

    let _first =
        beachcomber::singleton::SingletonLock::acquire(&pid_path, "0.5.1", TEST_HASH_A).unwrap();

    // Same hash and the probe reports the owner is serving → exit silently.
    let second = beachcomber::singleton::acquire_or_supersede_with(
        &AlwaysServing,
        Duration::ZERO,
        &pid_path,
        &socket_path,
        "0.5.1",
        TEST_HASH_A,
    )
    .expect("should succeed with Ok(None)");
    assert!(
        second.is_none(),
        "same hash + serving owner should return None"
    );
}

#[test]
fn acquire_or_supersede_same_hash_not_serving_does_not_exit_silent() {
    let tmpdir = tempfile::tempdir().unwrap();
    let pid_path = tmpdir.path().join("pid");
    let socket_path = tmpdir.path().join("sock");

    // The existing lock records *our* pid (SingletonLock::acquire writes it).
    let _first =
        beachcomber::singleton::SingletonLock::acquire(&pid_path, "0.5.1", TEST_HASH_A).unwrap();

    // Same hash but the probe reports the owner is NOT serving → supersede path,
    // NOT exit-silent. The supersede target is our own pid, so supersede_existing
    // refuses (self-guard) and the call returns Err — proving we did not silently
    // exit. The fast self-guard means zero grace resolves immediately.
    let r = beachcomber::singleton::acquire_or_supersede_with(
        &NeverServing,
        Duration::ZERO,
        &pid_path,
        &socket_path,
        "0.5.1",
        TEST_HASH_A,
    );
    assert!(
        !matches!(r, Ok(None)),
        "same hash + non-serving owner must take the supersede path, not exit silently"
    );
    assert!(r.is_err(), "supersede of self should surface an error");
}

#[test]
fn probe_until_serving_returns_true_when_serving() {
    let socket = Path::new("/nonexistent/sock");
    assert!(beachcomber::singleton::probe_until_serving(
        &AlwaysServing,
        socket,
        Duration::ZERO
    ));
}

#[test]
fn probe_until_serving_returns_false_when_never_serving_after_grace() {
    let socket = Path::new("/nonexistent/sock");
    assert!(!beachcomber::singleton::probe_until_serving(
        &NeverServing,
        socket,
        Duration::ZERO
    ));
}

#[test]
fn probe_until_serving_waits_through_grace_for_late_bind() {
    use std::sync::atomic::{AtomicU32, Ordering};

    // Serving only after the 3rd probe — simulates a winner that binds slightly
    // late (Example 2: the loser must wait, not kill the winner).
    struct ServingAfter(AtomicU32);
    impl SocketProbe for ServingAfter {
        fn is_serving(&self, _socket: &Path) -> bool {
            self.0.fetch_add(1, Ordering::SeqCst) >= 3
        }
    }

    let probe = ServingAfter(AtomicU32::new(0));
    let socket = Path::new("/nonexistent/sock");
    assert!(beachcomber::singleton::probe_until_serving(
        &probe,
        socket,
        Duration::from_secs(2)
    ));
}

// ---------------------------------------------------------------------------
// Orphan reaping — canon §"Orphan reaping" behaviour assertions
// ---------------------------------------------------------------------------

use beachcomber::boundaries::proc_table::{ProcessInfo, ProcessTable};
use beachcomber::singleton::policy::{ReapContext, ReapDecision, decide_reap, is_canonical_daemon};

fn daemon_proc(pid: u32, ppid: u32, age_secs: u64, extra: &[&str]) -> ProcessInfo {
    let mut argv = vec!["/some/path/comb".to_string(), "daemon".to_string()];
    argv.extend(extra.iter().map(|s| s.to_string()));
    ProcessInfo {
        pid,
        ppid,
        argv,
        age_secs,
    }
}

fn reap_ctx() -> ReapContext {
    ReapContext {
        our_pid: 100,
        our_socket: "/tmp/beachcomber-501/sock".into(),
        grace_age_secs: 60,
    }
}

#[test]
fn orphaned_side_daemon_is_reaped() {
    let p = daemon_proc(200, 1, 3600, &["--socket", "/tmp/.tmpXYZ/beachcomber/sock"]);
    assert_eq!(decide_reap(&p, &reap_ctx()), ReapDecision::Reap);
}

#[test]
fn exit_with_parent_daemon_is_exempt() {
    let p = daemon_proc(
        200,
        1,
        3600,
        &["--exit-with-parent", "--socket", "/tmp/.tmpXYZ/sock"],
    );
    assert!(matches!(
        decide_reap(&p, &reap_ctx()),
        ReapDecision::Exempt(_)
    ));
}

#[test]
fn no_reap_daemon_is_exempt() {
    let p = daemon_proc(200, 1, 3600, &["--no-reap", "--socket", "/tmp/side.sock"]);
    assert!(matches!(
        decide_reap(&p, &reap_ctx()),
        ReapDecision::Exempt(_)
    ));
}

#[test]
fn attended_daemon_is_exempt() {
    // Parent alive (PPID != 1) — a foreground debug run under a shell.
    let p = daemon_proc(200, 999, 3600, &["--socket", "/tmp/debug.sock"]);
    assert!(matches!(
        decide_reap(&p, &reap_ctx()),
        ReapDecision::Exempt(_)
    ));
}

#[test]
fn young_daemon_is_exempt() {
    let p = daemon_proc(200, 1, 10, &["--socket", "/tmp/.tmpXYZ/sock"]);
    assert!(matches!(
        decide_reap(&p, &reap_ctx()),
        ReapDecision::Exempt(_)
    ));
}

#[test]
fn reaper_itself_is_exempt() {
    let p = daemon_proc(100, 1, 3600, &["--socket", "/tmp/elsewhere.sock"]);
    assert!(matches!(
        decide_reap(&p, &reap_ctx()),
        ReapDecision::Exempt(_)
    ));
}

#[test]
fn same_socket_daemon_is_exempt() {
    // Startup-contention domain: flock + serving probe govern it, not reaping.
    let p = daemon_proc(200, 1, 3600, &["--socket", "/tmp/beachcomber-501/sock"]);
    assert!(matches!(
        decide_reap(&p, &reap_ctx()),
        ReapDecision::Exempt(_)
    ));

    let eq_form = daemon_proc(201, 1, 3600, &["--socket=/tmp/beachcomber-501/sock"]);
    assert!(matches!(
        decide_reap(&eq_form, &reap_ctx()),
        ReapDecision::Exempt(_)
    ));
}

#[test]
fn non_daemon_processes_are_ignored() {
    let vim = ProcessInfo {
        pid: 200,
        ppid: 1,
        argv: vec!["vim".into()],
        age_secs: 3600,
    };
    assert!(matches!(
        decide_reap(&vim, &reap_ctx()),
        ReapDecision::Exempt(_)
    ));

    let comb_get = ProcessInfo {
        pid: 201,
        ppid: 1,
        argv: vec![
            "/usr/local/bin/comb".into(),
            "get".into(),
            "git.branch".into(),
        ],
        age_secs: 3600,
    };
    assert!(matches!(
        decide_reap(&comb_get, &reap_ctx()),
        ReapDecision::Exempt(_)
    ));
}

#[test]
fn d_alias_is_matched() {
    let p = ProcessInfo {
        pid: 200,
        ppid: 1,
        argv: vec![
            "comb".into(),
            "d".into(),
            "--socket".into(),
            "/tmp/x.sock".into(),
        ],
        age_secs: 3600,
    };
    assert_eq!(decide_reap(&p, &reap_ctx()), ReapDecision::Reap);
}

#[test]
fn deleted_worktree_exe_is_still_matched() {
    // Reaping matches on argv, so a binary deleted from disk is still reapable.
    let p = ProcessInfo {
        pid: 200,
        ppid: 1,
        argv: vec![
            "/ws/repo/.worktrees/gone/target/debug/comb".into(),
            "daemon".into(),
            "--socket".into(),
            "/tmp/.tmpGone/beachcomber/sock".into(),
        ],
        age_secs: 86400 * 21,
    };
    assert_eq!(decide_reap(&p, &reap_ctx()), ReapDecision::Reap);
}

#[test]
fn canonicality_is_bound_socket_equals_own_resolution() {
    use std::path::Path;
    assert!(is_canonical_daemon(
        Path::new("/tmp/beachcomber-501/sock"),
        Path::new("/tmp/beachcomber-501/sock")
    ));
    assert!(!is_canonical_daemon(
        Path::new("/tmp/.tmpXYZ/beachcomber/sock"),
        Path::new("/tmp/beachcomber-501/sock")
    ));
}

struct FakeTable {
    procs: Vec<ProcessInfo>,
    pid1: bool,
}
impl FakeTable {
    fn new(procs: Vec<ProcessInfo>) -> Self {
        Self { procs, pid1: true }
    }
    fn confined(procs: Vec<ProcessInfo>) -> Self {
        Self { procs, pid1: false }
    }
}
impl ProcessTable for FakeTable {
    fn list_own(&self) -> Vec<ProcessInfo> {
        self.procs.clone()
    }
    fn pid1_visible(&self) -> bool {
        self.pid1
    }
}

#[test]
fn reap_sweep_kills_only_orphans() {
    use std::cell::RefCell;

    let table = FakeTable::new(vec![
        daemon_proc(200, 1, 3600, &["--socket", "/tmp/.tmpA/sock"]), // orphan
        daemon_proc(201, 1, 3600, &["--exit-with-parent"]),          // exempt
        daemon_proc(202, 999, 3600, &["--socket", "/tmp/dbg.sock"]), // attended
        daemon_proc(100, 1, 3600, &[]),                              // self
        daemon_proc(203, 1, 5, &["--socket", "/tmp/.tmpB/sock"]),    // young
    ]);
    let killed: RefCell<Vec<u32>> = RefCell::new(Vec::new());
    let kill = |pid: u32| {
        killed.borrow_mut().push(pid);
        Ok(())
    };

    let report = beachcomber::singleton::reap_sweep_with(&table, &kill, &|_| false, &reap_ctx());
    assert_eq!(report.reaped, vec![200]);
    assert_eq!(*killed.borrow(), vec![200]);
}

#[test]
fn reap_sweep_continues_past_kill_failures() {
    use std::cell::RefCell;

    let table = FakeTable::new(vec![
        daemon_proc(200, 1, 3600, &["--socket", "/tmp/.tmpA/sock"]),
        daemon_proc(300, 1, 3600, &["--socket", "/tmp/.tmpB/sock"]),
    ]);
    let attempts: RefCell<Vec<u32>> = RefCell::new(Vec::new());
    let kill = |pid: u32| {
        attempts.borrow_mut().push(pid);
        if pid == 200 {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "nope",
            ))
        } else {
            Ok(())
        }
    };

    let report = beachcomber::singleton::reap_sweep_with(&table, &kill, &|_| false, &reap_ctx());
    assert_eq!(*attempts.borrow(), vec![200, 300], "both orphans attempted");
    assert_eq!(
        report.reaped,
        vec![300],
        "only the successful kill is reported reaped"
    );
}

// --- SweepReport (canon invariant 13: every sweep leaves an accountable trace) ---

#[test]
fn sweep_report_tallies_rows_candidates_and_exemptions() {
    let table = FakeTable::new(vec![
        daemon_proc(200, 1, 3600, &["--socket", "/tmp/.tmpA/sock"]), // reaped
        daemon_proc(201, 1, 3600, &["--exit-with-parent"]),          // exempt rule 2
        daemon_proc(202, 999, 3600, &["--socket", "/tmp/dbg.sock"]), // attended (rule 4)
        daemon_proc(100, 1, 3600, &[]),                              // self (rule 1)
        daemon_proc(203, 1, 5, &["--socket", "/tmp/.tmpB/sock"]),    // young (rule 5)
        // A non-daemon row: enumerated but never a candidate.
        ProcessInfo {
            pid: 999,
            ppid: 1,
            argv: vec!["/bin/zsh".into()],
            age_secs: 10_000,
        },
    ]);
    let kill = |_pid: u32| Ok(());

    let report = beachcomber::singleton::reap_sweep_with(&table, &kill, &|_| false, &reap_ctx());
    assert_eq!(report.rows_enumerated, 6);
    assert_eq!(report.candidates, 5, "non-daemon rows are not candidates");
    assert_eq!(report.reaped, vec![200]);
    assert!(report.pid1_visible);
    let tally = |rule: &str| {
        report
            .exemptions
            .iter()
            .find(|(r, _)| *r == rule)
            .map(|(_, n)| *n)
            .unwrap_or(0)
    };
    assert_eq!(tally("self"), 1);
    assert_eq!(tally("exit-with-parent"), 1);
    assert_eq!(tally("attended (parent alive)"), 1);
    assert_eq!(tally("younger than grace age"), 1);
}

#[test]
fn sweep_report_classifies_denied_and_failed_kills() {
    let table = FakeTable::new(vec![
        daemon_proc(200, 1, 3600, &["--socket", "/tmp/.tmpA/sock"]), // EPERM
        daemon_proc(300, 1, 3600, &["--socket", "/tmp/.tmpB/sock"]), // other error
        daemon_proc(400, 1, 3600, &["--socket", "/tmp/.tmpC/sock"]), // ok
    ]);
    let kill = |pid: u32| match pid {
        200 => Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "sandbox says no",
        )),
        300 => Err(std::io::Error::other("boom")),
        _ => Ok(()),
    };

    let report = beachcomber::singleton::reap_sweep_with(&table, &kill, &|_| false, &reap_ctx());
    assert_eq!(report.kill_denied, 1, "EPERM counted as denied");
    assert_eq!(report.kill_failed, 1, "non-EPERM counted as failed");
    assert_eq!(report.reaped, vec![400]);
}

#[test]
fn sweep_report_surfaces_confined_visibility_but_still_reaps() {
    use std::cell::RefCell;

    let table = FakeTable::confined(vec![daemon_proc(
        200,
        1,
        3600,
        &["--socket", "/tmp/.tmpA/sock"],
    )]);
    let killed: RefCell<Vec<u32>> = RefCell::new(Vec::new());
    let kill = |pid: u32| {
        killed.borrow_mut().push(pid);
        Ok(())
    };

    let report = beachcomber::singleton::reap_sweep_with(&table, &kill, &|_| false, &reap_ctx());
    assert!(!report.pid1_visible, "confinement reported");
    assert_eq!(
        report.reaped,
        vec![200],
        "visible orphans still reaped while confined"
    );
}

#[test]
fn reaper_health_record_sweep_accumulates() {
    use beachcomber::singleton::{ReaperHealth, SweepReport};
    use std::sync::atomic::Ordering::Relaxed;

    let health = ReaperHealth::default();
    let report = SweepReport {
        rows_enumerated: 30,
        candidates: 3,
        reaped: vec![200, 300],
        kill_denied: 1,
        pid1_visible: true,
        ..SweepReport::default()
    };
    health.record_sweep(&report);
    health.record_sweep(&report);

    assert_eq!(health.sweeps_total.load(Relaxed), 2);
    assert_eq!(health.reaped_total.load(Relaxed), 4);
    assert_eq!(health.kill_denied_total.load(Relaxed), 2);
    assert!(health.visibility_ok.load(Relaxed));

    // Visibility reflects the LATEST sweep, not history.
    let confined = SweepReport {
        pid1_visible: false,
        ..SweepReport::default()
    };
    health.record_sweep(&confined);
    assert!(!health.visibility_ok.load(Relaxed));
}

// --- Corpse cleanup (canon §"Corpse cleanup", invariant 14) ---

#[test]
fn reap_sweep_calls_cleanup_with_orphan_socket_and_counts() {
    use std::cell::RefCell;

    let table = FakeTable::new(vec![daemon_proc(
        200,
        1,
        3600,
        &["--socket", "/tmp/.tmpA/sock"],
    )]);
    let kill = |_pid: u32| Ok(());
    let cleaned: RefCell<Vec<std::path::PathBuf>> = RefCell::new(Vec::new());
    let cleanup = |socket: &std::path::Path| {
        cleaned.borrow_mut().push(socket.to_path_buf());
        true
    };

    let report = beachcomber::singleton::reap_sweep_with(&table, &kill, &cleanup, &reap_ctx());
    assert_eq!(
        *cleaned.borrow(),
        vec![std::path::PathBuf::from("/tmp/.tmpA/sock")],
        "cleanup runs against the reaped orphan's socket"
    );
    assert_eq!(report.corpses_unlinked, 1);
}

#[test]
fn reap_sweep_skips_cleanup_when_kill_fails() {
    use std::cell::RefCell;

    let table = FakeTable::new(vec![daemon_proc(
        200,
        1,
        3600,
        &["--socket", "/tmp/.tmpA/sock"],
    )]);
    let kill = |_pid: u32| {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "no",
        ))
    };
    let cleanup_called = RefCell::new(false);
    let cleanup = |_: &std::path::Path| {
        *cleanup_called.borrow_mut() = true;
        true
    };

    let report = beachcomber::singleton::reap_sweep_with(&table, &kill, &cleanup, &reap_ctx());
    assert!(
        !*cleanup_called.borrow(),
        "no cleanup for a process that was not killed"
    );
    assert_eq!(report.corpses_unlinked, 0);
}

struct FakeProbe(bool);
impl beachcomber::boundaries::socket::SocketProbe for FakeProbe {
    fn is_serving(&self, _socket: &std::path::Path) -> bool {
        self.0
    }
}

#[test]
fn cleanup_orphan_corpse_removes_socket_and_pid_files() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("sock");
    std::fs::write(&sock, b"").unwrap();
    std::fs::write(tmp.path().join("pid"), b"{}").unwrap();
    std::fs::write(tmp.path().join("daemon.pid"), b"123").unwrap();

    let removed = beachcomber::singleton::cleanup_orphan_corpse(&FakeProbe(false), &sock);
    assert!(removed);
    assert!(!sock.exists(), "socket corpse removed");
    assert!(!tmp.path().join("pid").exists(), "pid file removed");
    assert!(
        !tmp.path().join("daemon.pid").exists(),
        "daemon.pid removed"
    );
}

#[test]
fn cleanup_orphan_corpse_never_unlinks_serving_socket() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("sock");
    std::fs::write(&sock, b"").unwrap();
    std::fs::write(tmp.path().join("pid"), b"{}").unwrap();

    let removed = beachcomber::singleton::cleanup_orphan_corpse(&FakeProbe(true), &sock);
    assert!(!removed);
    assert!(sock.exists(), "serving socket must be left in place");
    assert!(tmp.path().join("pid").exists(), "pid file left in place");
}

// --- Reaper-role resolution ignores $BEACHCOMBER_SOCKET (canon §"Who reaps") ---

#[test]
fn reaper_resolution_ignores_beachcomber_socket_env() {
    let cfg = Config::default();
    let path = temp_env::with_vars(
        [("BEACHCOMBER_SOCKET", Some("/custom/override/sock"))],
        || cfg.resolve_reaper_socket_path(),
    );
    let s = path.to_string_lossy();
    assert!(
        s.starts_with("/tmp/beachcomber-") && s.ends_with("/sock"),
        "reaper resolution must ignore env override, got {s}"
    );
}

#[test]
fn reaper_resolution_honors_config_override() {
    let mut cfg = Config::default();
    cfg.daemon.socket_path = Some("/etc/comb/sock".into());
    assert_eq!(
        cfg.resolve_reaper_socket_path(),
        PathBuf::from("/etc/comb/sock")
    );
}

#[test]
fn resolve_socket_path_reports_source() {
    use beachcomber::config::SocketPathSource;

    let mut cfg = Config::default();
    cfg.daemon.socket_path = Some("/etc/comb/sock".into());
    assert_eq!(
        cfg.resolve_socket_path_with_source().1,
        SocketPathSource::ConfigOverride
    );

    let cfg = Config::default();
    let (_, source) = temp_env::with_vars([("BEACHCOMBER_SOCKET", Some("/custom/sock"))], || {
        cfg.resolve_socket_path_with_source()
    });
    assert_eq!(source, SocketPathSource::EnvVar);

    let (_, source) = temp_env::with_vars([("BEACHCOMBER_SOCKET", None::<&str>)], || {
        cfg.resolve_socket_path_with_source()
    });
    assert_eq!(source, SocketPathSource::Default);
}
