/// Integration tests for `crate::daemon` pure-policy and injection-point variants.
///
/// Pure helpers are tested inline inside `lifecycle.rs`; these tests cover the
/// OS-bound wrapper `ensure_daemon_with` via hand-written test doubles for
/// `DaemonSpawner`.  We avoid pulling mockall into integration tests because
/// `MockDaemonSpawner` is only generated under `cfg(test)` (inside the crate),
/// and mockall is a dev-dependency — not available to external test binaries.
use beachcomber::boundaries::spawn::DaemonSpawner;
use beachcomber::daemon::{ensure_daemon_with, lifecycle::pid_path_for_socket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

/// A spawner that never forks and always reports the socket as ready.
struct AlwaysRunningSpawner;

impl DaemonSpawner for AlwaysRunningSpawner {
    fn fork_daemon(&self, _binary: &str, _socket: &Path, _no_reap: bool) -> std::io::Result<()> {
        panic!("fork_daemon should never be called when daemon is already running");
    }

    fn wait_for_socket(&self, _socket: &Path, _attempts: u32) -> bool {
        panic!("wait_for_socket should never be called when daemon is already running");
    }
}

/// A spawner that records whether it was called and controls the wait result.
struct RecordingSpawner {
    fork_called: Arc<AtomicBool>,
    fork_no_reap: Arc<AtomicBool>,
    wait_called: Arc<AtomicU32>,
    wait_returns: bool,
}

impl RecordingSpawner {
    fn new(wait_returns: bool) -> (Self, Arc<AtomicBool>, Arc<AtomicU32>) {
        let fork_called = Arc::new(AtomicBool::new(false));
        let wait_called = Arc::new(AtomicU32::new(0));
        let spawner = Self {
            fork_called: fork_called.clone(),
            fork_no_reap: Arc::new(AtomicBool::new(false)),
            wait_called: wait_called.clone(),
            wait_returns,
        };
        (spawner, fork_called, wait_called)
    }
}

impl DaemonSpawner for RecordingSpawner {
    fn fork_daemon(&self, _binary: &str, _socket: &Path, no_reap: bool) -> std::io::Result<()> {
        self.fork_no_reap.store(no_reap, Ordering::SeqCst);
        self.fork_called.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn wait_for_socket(&self, _socket: &Path, _attempts: u32) -> bool {
        self.wait_called.fetch_add(1, Ordering::SeqCst);
        self.wait_returns
    }
}

// ---------------------------------------------------------------------------
// Pure: pid_path_for_socket
// ---------------------------------------------------------------------------

#[test]
fn pid_path_replaces_extension() {
    // Canonical case from the task spec.
    let p = pid_path_for_socket(&PathBuf::from("/tmp/foo.sock"));
    assert_eq!(p, PathBuf::from("/tmp/daemon.pid"));
}

#[test]
fn pid_path_replaces_directory_socket_name() {
    let p = pid_path_for_socket(&PathBuf::from("/tmp/beachcomber-501/sock"));
    assert_eq!(p, PathBuf::from("/tmp/beachcomber-501/daemon.pid"));
}

// ---------------------------------------------------------------------------
// Pure: WaitDecision
// ---------------------------------------------------------------------------

#[test]
fn wait_decision_terminates_at_max() {
    use beachcomber::daemon::lifecycle::{WaitDecision, next_wait_decision};
    // When attempt equals max_attempts the budget is exhausted.
    let decision = next_wait_decision(8, 8, 500);
    assert_eq!(decision, WaitDecision::Timeout);
}

#[test]
fn wait_decision_sleeps_before_max() {
    use beachcomber::daemon::lifecycle::{WaitDecision, next_wait_decision};
    let decision = next_wait_decision(0, 8, 10);
    assert_eq!(decision, WaitDecision::Sleep(20));
}

// ---------------------------------------------------------------------------
// ensure_daemon_with — injected-spawner tests
// ---------------------------------------------------------------------------

/// When the daemon is already running, `fork_daemon` must NOT be called.
///
/// We bind a real UnixListener so `is_daemon_running` returns true.
#[test]
fn ensure_daemon_returns_quickly_if_already_running() {
    use std::os::unix::net::UnixListener;
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let sock_path = tmp.path().join("sock");

    // Bind a listener so the socket exists and is connectable.
    let _listener = UnixListener::bind(&sock_path).unwrap();

    // AlwaysRunningSpawner panics if fork_daemon or wait_for_socket is called.
    let result = ensure_daemon_with(&AlwaysRunningSpawner, &sock_path, false);
    assert!(
        result.is_ok(),
        "ensure_daemon_with should succeed: {result:?}"
    );
}

/// When no daemon is running, `fork_daemon` is called once and `wait_for_socket`
/// returns true — the overall call succeeds.
#[test]
fn ensure_daemon_forks_if_socket_missing() {
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let sock_path = tmp.path().join("no-such-sock");

    // Socket does not exist, so is_daemon_running returns false.
    let (spawner, fork_called, wait_called) = RecordingSpawner::new(true);

    let result = ensure_daemon_with(&spawner, &sock_path, false);
    assert!(
        result.is_ok(),
        "ensure_daemon_with should succeed: {result:?}"
    );
    assert!(
        fork_called.load(Ordering::SeqCst),
        "fork_daemon should have been called"
    );
    assert_eq!(
        wait_called.load(Ordering::SeqCst),
        1,
        "wait_for_socket should have been called exactly once"
    );
}

/// When fork succeeds but the daemon never becomes reachable, the call returns
/// a `TimedOut` error.
#[test]
fn ensure_daemon_errors_when_wait_times_out() {
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let sock_path = tmp.path().join("no-such-sock");

    let (spawner, fork_called, _wait_called) = RecordingSpawner::new(false);

    let result = ensure_daemon_with(&spawner, &sock_path, false);
    assert!(result.is_err(), "should return an error on timeout");
    assert_eq!(
        result.unwrap_err().kind(),
        std::io::ErrorKind::TimedOut,
        "error kind should be TimedOut"
    );
    assert!(
        fork_called.load(Ordering::SeqCst),
        "fork_daemon should have been called"
    );
}

/// Canon singleton.md §"Env-override spawns are flagged": the no_reap flag
/// passes through ensure_daemon_with to the spawner verbatim.
#[test]
fn ensure_daemon_threads_no_reap_flag_to_fork() {
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let sock_path = tmp.path().join("no-such-sock");

    let (spawner, fork_called, _wait) = RecordingSpawner::new(true);
    let no_reap_seen = spawner.fork_no_reap.clone();

    ensure_daemon_with(&spawner, &sock_path, true).unwrap();
    assert!(fork_called.load(Ordering::SeqCst));
    assert!(
        no_reap_seen.load(Ordering::SeqCst),
        "no_reap=true must reach fork_daemon"
    );
}

/// Pre-flight SUN_LEN guard: a socket path the kernel cannot bind is rejected
/// before forking a doomed daemon.
#[test]
fn ensure_daemon_rejects_overlong_socket_path_without_forking() {
    let long = format!("/tmp/{}/sock", "x".repeat(120));
    let (spawner, fork_called, _wait) = RecordingSpawner::new(true);

    let result = ensure_daemon_with(&spawner, Path::new(&long), false);
    let err = result.expect_err("overlong path must be rejected");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        !fork_called.load(Ordering::SeqCst),
        "must not fork a daemon doomed to fail bind"
    );
}
