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

struct FakeTable(Vec<ProcessInfo>);
impl ProcessTable for FakeTable {
    fn list_own(&self) -> Vec<ProcessInfo> {
        self.0.clone()
    }
}

#[test]
fn reap_sweep_kills_only_orphans() {
    use std::cell::RefCell;

    let table = FakeTable(vec![
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

    let reaped = beachcomber::singleton::reap_sweep_with(&table, &kill, &reap_ctx());
    assert_eq!(reaped, vec![200]);
    assert_eq!(*killed.borrow(), vec![200]);
}

#[test]
fn reap_sweep_continues_past_kill_failures() {
    use std::cell::RefCell;

    let table = FakeTable(vec![
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

    let reaped = beachcomber::singleton::reap_sweep_with(&table, &kill, &reap_ctx());
    assert_eq!(*attempts.borrow(), vec![200, 300], "both orphans attempted");
    assert_eq!(
        reaped,
        vec![300],
        "only the successful kill is reported reaped"
    );
}
