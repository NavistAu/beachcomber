//! Pure decision logic for the daemon lifecycle — no OS calls, fully unit-testable.
//!
//! Functions here operate only on their arguments and return plain values.
//! Starting a daemon is `libbeachcomber::spawn`'s.

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
    fn reparent_during_startup_triggers_exit_when_armed() {
        // The ppid captured before startup is the real parent; by the time the
        // watch is armed the parent is gone and we already sit under pid 1.
        assert!(should_exit_on_reparent(4321, 1));
        // The old late capture saw 1 == 1 and never fired.
        assert!(!should_exit_on_reparent(1, 1));
    }
}
