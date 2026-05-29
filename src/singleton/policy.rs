//! Pure decision logic for the singleton enforcement layer — no OS calls, fully unit-testable.
//!
//! Functions here operate only on their arguments and return plain values.
//! OS-bound work (filesystem access, process signaling) lives in
//! `crate::singleton` (`src/singleton/mod.rs`).

use crate::singleton::PidFileRecord;

/// Outcome of a supersession check.
///
/// See `decide_supersession` for the full decision rule.
pub use crate::singleton::SupersessionDecision;

/// Given the existing singleton's record, our own binary hash, and whether the
/// existing owner is serving its socket, decide whether to supersede (kill and
/// take over) or exit silently (existing daemon is fine).
///
/// Different hash → supersede. Same hash + serving → exit silently. Same hash but
/// not serving → supersede (owner wedged before bind, or socket deleted).
///
/// This is the same function re-exported from `crate::singleton` for callers that
/// want to import from the pure-policy module explicitly.
pub fn decide_supersession(
    existing: &PidFileRecord,
    our_binary_hash: &str,
    owner_serving: bool,
) -> SupersessionDecision {
    crate::singleton::decide_supersession(existing, our_binary_hash, owner_serving)
}

/// Given a file's last-modified timestamp (in milliseconds since the Unix epoch) and
/// the process-start timestamp (also in milliseconds since the Unix epoch), decide
/// whether the binary is strictly newer than the process start time.
///
/// Returns `true` when the file was modified *after* the process started, meaning the
/// on-disk binary has changed and the daemon should restart.
///
/// This function is the pure comparison half of `binary_newer_than`; the OS-bound half
/// (reading the file's mtime via `std::fs::metadata`) stays in `crate::singleton`.
pub fn is_binary_newer(mtime_unix_ms: u64, process_start_unix_ms: u64) -> bool {
    mtime_unix_ms > process_start_unix_ms
}

/// Decide whether a pid-file record describes a stale entry that can be reclaimed.
///
/// A record is considered stale (and safe to reclaim) when the process is **not**
/// running (`process_alive` = false).  When the process is alive the record is fresh
/// regardless of any other field.
///
/// The OS-bound caller is responsible for checking liveness (e.g., `kill(pid, 0)`)
/// and supplying the boolean.
pub fn is_pidfile_stale(process_alive: bool) -> bool {
    !process_alive
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::singleton::{PidFileRecord, SupersessionDecision};

    const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn make_record(pid: u32, hash: &str) -> PidFileRecord {
        PidFileRecord {
            pid,
            version: "0.5.1".into(),
            binary: "/path/to/comb".into(),
            binary_hash: hash.into(),
            started_unix_ms: 0,
        }
    }

    // --- decide_supersession ---

    #[test]
    fn same_hash_serving_means_exit_silent() {
        let rec = make_record(1234, HASH_A);
        let decision = decide_supersession(&rec, HASH_A, true);
        assert!(
            matches!(decision, SupersessionDecision::ExitSilent),
            "same hash + serving should yield ExitSilent, got {decision:?}"
        );
    }

    #[test]
    fn same_hash_not_serving_means_supersede() {
        let rec = make_record(1234, HASH_A);
        let decision = decide_supersession(&rec, HASH_A, false);
        match decision {
            SupersessionDecision::Supersede { existing_pid } => {
                assert_eq!(existing_pid, 1234, "should carry the existing PID");
            }
            other => panic!("expected Supersede for non-serving owner, got {other:?}"),
        }
    }

    #[test]
    fn different_hash_means_supersede() {
        let rec = make_record(1234, HASH_A);
        // Serving state is irrelevant when the build differs.
        let decision = decide_supersession(&rec, HASH_B, true);
        match decision {
            SupersessionDecision::Supersede { existing_pid } => {
                assert_eq!(existing_pid, 1234, "should carry the existing PID");
            }
            other => panic!("expected Supersede, got {other:?}"),
        }
    }

    #[test]
    fn same_version_different_hash_still_supersedes() {
        // Two dev builds at the same cargo version but different binaries —
        // human version matches, but binary_hash differs, so we supersede.
        let mut rec = make_record(9999, HASH_A);
        rec.version = "0.5.1".into(); // same version string
        let decision = decide_supersession(&rec, HASH_B, true);
        assert!(
            matches!(decision, SupersessionDecision::Supersede { .. }),
            "version string is advisory only; hash difference must supersede"
        );
    }

    #[test]
    fn supersede_carries_existing_pid_correctly() {
        let rec = make_record(42, HASH_A);
        let decision = decide_supersession(&rec, HASH_B, true);
        if let SupersessionDecision::Supersede { existing_pid } = decision {
            assert_eq!(existing_pid, 42);
        } else {
            panic!("expected Supersede");
        }
    }

    // --- is_binary_newer ---

    #[test]
    fn binary_newer_when_mtime_after_start() {
        assert!(is_binary_newer(1000, 999));
    }

    #[test]
    fn binary_not_newer_when_mtime_equals_start() {
        assert!(!is_binary_newer(1000, 1000));
    }

    #[test]
    fn binary_not_newer_when_mtime_before_start() {
        assert!(!is_binary_newer(999, 1000));
    }

    #[test]
    fn binary_newer_handles_zero_start() {
        // Any reasonable mtime is newer than epoch=0.
        assert!(is_binary_newer(1, 0));
    }

    #[test]
    fn binary_not_newer_at_exact_epoch() {
        assert!(!is_binary_newer(0, 0));
    }

    // --- is_pidfile_stale ---

    #[test]
    fn stale_when_process_gone() {
        assert!(is_pidfile_stale(false));
    }

    #[test]
    fn fresh_when_process_alive() {
        assert!(!is_pidfile_stale(true));
    }
}
