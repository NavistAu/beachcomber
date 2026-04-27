//! Pure decision logic for the daemon lifecycle — no OS calls, fully unit-testable.
//!
//! Functions here operate only on their arguments and return plain values.
//! OS-bound work (filesystem access, socket polling, process spawning) lives in
//! `crate::daemon` (`src/daemon/mod.rs`).

use std::path::{Path, PathBuf};

/// Derive the PID-file path that corresponds to a given socket path.
///
/// The PID file always lives in the same directory as the socket, named `daemon.pid`.
///
/// ```
/// # use std::path::{Path, PathBuf};
/// # use beachcomber::daemon::lifecycle::pid_path_for_socket;
/// let p = pid_path_for_socket(Path::new("/tmp/beachcomber-501/sock"));
/// assert_eq!(p, PathBuf::from("/tmp/beachcomber-501/daemon.pid"));
/// ```
pub fn pid_path_for_socket(socket_path: &Path) -> PathBuf {
    socket_path.with_file_name("daemon.pid")
}

/// The outcome of a single wait-poll iteration.
#[derive(Debug, PartialEq, Eq)]
pub enum WaitDecision {
    /// The socket is reachable; stop waiting.
    Ready,
    /// Not yet reachable; sleep for the given number of milliseconds then try again.
    Sleep(u64),
    /// Attempt budget exhausted; give up.
    Timeout,
}

/// Decide what to do after a failed socket-connectivity attempt.
///
/// `attempt` is 0-based (the first attempt that failed is `0`).
/// `max_attempts` is the total budget; once `attempt >= max_attempts - 1` (meaning we
/// have used the last slot) the decision is `Timeout`.
///
/// The delay follows an exponential backoff starting at 10 ms, doubling each time,
/// capped at 500 ms.
pub fn next_wait_decision(attempt: u32, max_attempts: u32, current_delay_ms: u64) -> WaitDecision {
    if attempt >= max_attempts {
        return WaitDecision::Timeout;
    }
    let next_delay = (current_delay_ms * 2).min(500);
    WaitDecision::Sleep(next_delay)
}

/// Decide whether `ensure_daemon` needs to fork.
///
/// Returns `true` when the daemon is **not** running and a fork is required.
/// Pure: the caller supplies the `is_running` boolean obtained from an OS check.
pub fn needs_fork(is_running: bool) -> bool {
    !is_running
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // --- pid_path_for_socket ---

    #[test]
    fn pid_path_replaces_filename_with_daemon_pid() {
        let sock = PathBuf::from("/tmp/beachcomber-501/sock");
        assert_eq!(
            pid_path_for_socket(&sock),
            PathBuf::from("/tmp/beachcomber-501/daemon.pid")
        );
    }

    #[test]
    fn pid_path_for_deep_socket() {
        let sock = PathBuf::from("/run/user/1000/beachcomber/sock");
        assert_eq!(
            pid_path_for_socket(&sock),
            PathBuf::from("/run/user/1000/beachcomber/daemon.pid")
        );
    }

    #[test]
    fn pid_path_for_socket_with_extension() {
        // Socket files can have an extension; we replace the whole filename.
        let sock = PathBuf::from("/tmp/foo.sock");
        assert_eq!(pid_path_for_socket(&sock), PathBuf::from("/tmp/daemon.pid"));
    }

    // --- next_wait_decision ---

    #[test]
    fn wait_decision_first_attempt_sleeps() {
        let d = next_wait_decision(0, 8, 10);
        assert_eq!(d, WaitDecision::Sleep(20));
    }

    #[test]
    fn wait_decision_caps_at_500ms() {
        let d = next_wait_decision(5, 8, 400);
        assert_eq!(d, WaitDecision::Sleep(500));
    }

    #[test]
    fn wait_decision_terminates_at_max() {
        // attempt == max_attempts means we have exhausted the budget.
        let d = next_wait_decision(8, 8, 500);
        assert_eq!(d, WaitDecision::Timeout);
    }

    #[test]
    fn wait_decision_last_valid_attempt_still_sleeps() {
        // attempt == max_attempts - 1 is the last valid slot; still returns Sleep.
        let d = next_wait_decision(7, 8, 500);
        assert_eq!(d, WaitDecision::Sleep(500));
    }

    // --- needs_fork ---

    #[test]
    fn needs_fork_when_not_running() {
        assert!(needs_fork(false));
    }

    #[test]
    fn no_fork_when_already_running() {
        assert!(!needs_fork(true));
    }
}
