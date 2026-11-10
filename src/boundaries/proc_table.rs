//! ProcessTable boundary trait — abstracts uid-owned process enumeration from OS state.
//!
//! Used by the singleton's orphan reaping (see
//! `website/docs/internals/architecture.md` §7). The reader is
//! [`libbeachcomber::proc`], which the `pgrep` template filter shares; this
//! trait is the seam a test mocks.

pub use libbeachcomber::proc::ProcessInfo;

#[cfg_attr(test, mockall::automock)]
pub trait ProcessTable: Send + Sync {
    /// Processes owned by the current real uid. Entries whose metadata or argv
    /// cannot be read (permission, zombie, raced exit) are omitted.
    fn list_own(&self) -> Vec<ProcessInfo>;

    /// Reaper visibility self-test: true when PID 1
    /// appears in the RAW enumeration (before uid filtering). False means the
    /// process table is confined (sandbox, hidepid) and reaping is blind to
    /// processes outside the confinement.
    fn pid1_visible(&self) -> bool;
}

pub struct RealProcessTable;

impl ProcessTable for RealProcessTable {
    fn list_own(&self) -> Vec<ProcessInfo> {
        libbeachcomber::proc::list_own()
    }

    fn pid1_visible(&self) -> bool {
        libbeachcomber::proc::pid1_visible()
    }
}
