//! ProcessKiller boundary trait — abstracts signal delivery and process enumeration
//! for the singleton orphan-reaping path.

use std::path::Path;

/// Trait for the two OS operations that `reap_orphans` needs.
///
/// The real implementation uses `sysinfo` for enumeration and `libc::kill` for
/// signaling.  Tests inject a hand-written double that records calls without
/// touching the OS.
#[cfg_attr(test, mockall::automock)]
pub trait ProcessKiller: Send + Sync {
    /// Return the PIDs of all processes whose binary path canonicalises to
    /// `our_exe`, excluding `our_pid` (the current process).
    fn list_by_exe(&self, our_exe: &Path, our_pid: u32) -> Vec<u32>;

    /// Send SIGTERM to `pid`, wait up to `grace_ms` milliseconds for graceful exit,
    /// then SIGKILL if still alive.  Returns `Ok(())` once the target is gone.
    /// It is NOT an error if the target is already dead.
    fn kill_gracefully(&self, pid: u32, grace_ms: u64) -> std::io::Result<()>;
}

pub struct RealProcessKiller;

impl ProcessKiller for RealProcessKiller {
    fn list_by_exe(&self, our_exe: &Path, our_pid: u32) -> Vec<u32> {
        crate::singleton::find_orphan_daemons_raw(our_exe, our_pid)
    }

    fn kill_gracefully(&self, pid: u32, grace_ms: u64) -> std::io::Result<()> {
        crate::singleton::supersede_existing(pid, std::time::Duration::from_millis(grace_ms))
    }
}
