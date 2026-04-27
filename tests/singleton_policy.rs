//! Integration tests for `crate::singleton` pure-policy and injection-point variants.
//!
//! Pure helpers (`decide_supersession`, `is_binary_newer`, `is_pidfile_stale`,
//! `filter_orphan_pids`) are tested inline in `singleton/policy.rs`.  These tests
//! cover the OS-bound wrappers via hand-written test doubles for `ProcessKiller`.
//!
//! We avoid importing `MockProcessKiller` from the crate because mockall mocks are
//! only generated inside the crate's own `cfg(test)`, not in external test binaries.

use beachcomber::boundaries::killer::ProcessKiller;
use beachcomber::singleton::reap_orphans_with;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

/// A killer that returns a fixed list of PIDs and records whether `kill_gracefully`
/// was called for each one.
struct RecordingKiller {
    /// PIDs reported by `list_by_exe` (already excludes `our_pid` — the
    /// production implementation does the filtering, but here we control the list).
    reported_pids: Vec<u32>,
    /// Number of times `kill_gracefully` was called.
    kill_count: Arc<AtomicUsize>,
    /// Whether `kill_gracefully` should succeed.
    kill_succeeds: bool,
}

impl RecordingKiller {
    fn new(reported_pids: Vec<u32>, kill_succeeds: bool) -> (Self, Arc<AtomicUsize>) {
        let kill_count = Arc::new(AtomicUsize::new(0));
        let killer = Self {
            reported_pids,
            kill_count: kill_count.clone(),
            kill_succeeds,
        };
        (killer, kill_count)
    }
}

impl ProcessKiller for RecordingKiller {
    fn list_by_exe(&self, _our_exe: &Path, _our_pid: u32) -> Vec<u32> {
        self.reported_pids.clone()
    }

    fn kill_gracefully(&self, _pid: u32, _grace_ms: u64) -> std::io::Result<()> {
        self.kill_count.fetch_add(1, Ordering::SeqCst);
        if self.kill_succeeds {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "test: kill refused",
            ))
        }
    }
}

/// A killer that asserts `list_by_exe` is never called with our own pid.
struct SelfExclusionAssertingKiller {
    our_pid: u32,
    was_called: Arc<AtomicBool>,
}

impl ProcessKiller for SelfExclusionAssertingKiller {
    fn list_by_exe(&self, _our_exe: &Path, our_pid: u32) -> Vec<u32> {
        self.was_called.store(true, Ordering::SeqCst);
        assert_eq!(
            our_pid, self.our_pid,
            "list_by_exe must receive the current process's pid"
        );
        // Return a list that contains our_pid as an extra entry — the real
        // implementation would exclude it, but here we verify reap_orphans_with
        // itself doesn't pass self-pid to kill_gracefully if the list happens to
        // contain it (because list_by_exe is supposed to already exclude it).
        // In production the killer excludes self; we model that here by returning
        // an empty list from our own pid's perspective.
        vec![]
    }

    fn kill_gracefully(&self, pid: u32, _grace_ms: u64) -> std::io::Result<()> {
        panic!("kill_gracefully should not be called when list is empty (pid={pid})");
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// `reap_orphans_with` must pass the current process's PID to `list_by_exe` so
/// the implementation can exclude self.
#[test]
fn reap_orphans_passes_self_pid_to_list() {
    let our_pid = std::process::id();
    let was_called = Arc::new(AtomicBool::new(false));
    let killer = SelfExclusionAssertingKiller {
        our_pid,
        was_called: was_called.clone(),
    };

    let our_exe = std::env::current_exe().unwrap();
    let count = reap_orphans_with(&killer, &our_exe);

    assert!(
        was_called.load(Ordering::SeqCst),
        "list_by_exe must be called"
    );
    assert_eq!(count, 0, "no pids returned so nothing reaped");
}

/// When `list_by_exe` returns zero PIDs, `reap_orphans_with` returns 0 and never
/// calls `kill_gracefully`.
#[test]
fn reap_orphans_no_orphans_calls_no_kills() {
    let (killer, kill_count) = RecordingKiller::new(vec![], true);
    let our_exe = std::env::current_exe().unwrap();
    let count = reap_orphans_with(&killer, &our_exe);

    assert_eq!(count, 0);
    assert_eq!(kill_count.load(Ordering::SeqCst), 0);
}

/// When the list contains three orphan PIDs and all kills succeed, the count is 3
/// and `kill_gracefully` is called exactly once per PID.
#[test]
fn reap_orphans_kills_each_orphan_once() {
    let orphan_pids = vec![1001u32, 1002, 1003];
    let (killer, kill_count) = RecordingKiller::new(orphan_pids, true);
    let our_exe = std::env::current_exe().unwrap();

    let count = reap_orphans_with(&killer, &our_exe);

    assert_eq!(count, 3, "all three orphans should be counted as reaped");
    assert_eq!(
        kill_count.load(Ordering::SeqCst),
        3,
        "kill_gracefully should be called once per orphan"
    );
}

/// When `kill_gracefully` fails for all PIDs, the count is 0 (failures are
/// logged-and-continued, not propagated as a hard error).
#[test]
fn reap_orphans_counts_only_successful_kills() {
    let orphan_pids = vec![2001u32, 2002];
    let (killer, kill_count) = RecordingKiller::new(orphan_pids, false);
    let our_exe = std::env::current_exe().unwrap();

    let count = reap_orphans_with(&killer, &our_exe);

    assert_eq!(count, 0, "failed kills should not increment the count");
    assert_eq!(
        kill_count.load(Ordering::SeqCst),
        2,
        "kill_gracefully should still be attempted for each pid"
    );
}

/// When the list contains a mix of successes and failures, only the successes are counted.
#[test]
fn reap_orphans_partial_success() {
    struct PartialKiller {
        pids: Vec<u32>,
        fail_pid: u32,
        kill_count: Arc<AtomicUsize>,
    }

    impl ProcessKiller for PartialKiller {
        fn list_by_exe(&self, _our_exe: &Path, _our_pid: u32) -> Vec<u32> {
            self.pids.clone()
        }

        fn kill_gracefully(&self, pid: u32, _grace_ms: u64) -> std::io::Result<()> {
            self.kill_count.fetch_add(1, Ordering::SeqCst);
            if pid == self.fail_pid {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "test: refusing one pid",
                ))
            } else {
                Ok(())
            }
        }
    }

    let kill_count = Arc::new(AtomicUsize::new(0));
    let killer = PartialKiller {
        pids: vec![3001u32, 3002, 3003],
        fail_pid: 3002,
        kill_count: kill_count.clone(),
    };

    let our_exe = std::env::current_exe().unwrap();
    let count = reap_orphans_with(&killer, &our_exe);

    assert_eq!(count, 2, "only the two successful kills should be counted");
    assert_eq!(kill_count.load(Ordering::SeqCst), 3, "all three attempted");
}

// ---------------------------------------------------------------------------
// Pure-policy helpers (smoke-tested here for integration completeness;
// the full unit suite lives in singleton/policy.rs)
// ---------------------------------------------------------------------------

#[test]
fn policy_is_binary_newer_smoke() {
    use beachcomber::singleton::policy::is_binary_newer;
    assert!(is_binary_newer(100, 99));
    assert!(!is_binary_newer(99, 100));
    assert!(!is_binary_newer(100, 100));
}

#[test]
fn policy_is_pidfile_stale_smoke() {
    use beachcomber::singleton::policy::is_pidfile_stale;
    assert!(is_pidfile_stale(false));
    assert!(!is_pidfile_stale(true));
}

#[test]
fn policy_filter_orphan_pids_smoke() {
    use beachcomber::singleton::policy::filter_orphan_pids;
    let pids = vec![10u32, 20, 30];
    let filtered = filter_orphan_pids(&pids, 20);
    assert_eq!(filtered, vec![10, 30]);
}
