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

/// Decide whether a config-file change should trigger a graceful restart,
/// given the outcome of validating the new file's content (parse it, surface
/// the error rather than falling back to defaults). Mirrors the binary
/// self-watch's "any change restarts" policy, but gated: restarting into a
/// config that fails to parse would just crash-loop the freshly restarted
/// daemon (canon singleton.md §"Self-supervision"). Pure so the policy is
/// unit-tested independently of the file read in
/// `crate::daemon::config_watch`.
pub fn should_restart_for_config_change<T, E>(parse_result: &Result<T, E>) -> bool {
    parse_result.is_ok()
}

/// True when an `--exit-with-parent` daemon should shut down because it has been
/// re-parented: its parent pid changed from the value captured at startup,
/// meaning the original parent (the process that spawned it) has died and the
/// kernel re-parented the daemon (to launchd/init). Pure so the watch loop in
/// `cli::commands::daemon` stays trivial and this rule is unit-tested.
pub fn should_exit_on_reparent(initial_ppid: i32, current_ppid: i32) -> bool {
    current_ppid != initial_ppid
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

    // --- should_exit_on_reparent ---

    #[test]
    fn reparent_unchanged_ppid_keeps_running() {
        // Parent still alive (ppid unchanged) -> keep running.
        assert!(!should_exit_on_reparent(4321, 4321));
    }

    #[test]
    fn reparent_changed_ppid_triggers_exit() {
        // Original parent died -> re-parented to launchd (pid 1) -> exit.
        assert!(should_exit_on_reparent(4321, 1));
    }

    #[test]
    fn wait_decision_last_valid_attempt_still_sleeps() {
        // attempt == max_attempts - 1 is the last valid slot; still returns Sleep.
        let d = next_wait_decision(7, 8, 500);
        assert_eq!(d, WaitDecision::Sleep(500));
    }

    // --- should_restart_for_config_change ---

    #[test]
    fn config_parse_success_triggers_restart() {
        assert!(should_restart_for_config_change::<(), String>(&Ok(())));
    }

    #[test]
    fn config_parse_failure_keeps_running() {
        assert!(!should_restart_for_config_change::<(), String>(&Err(
            "invalid TOML".to_string()
        )));
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
