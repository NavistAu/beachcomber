//! ProcessSnapshotter boundary trait — abstracts process-spawn observation.

#[cfg_attr(test, mockall::automock)]
pub trait ProcessSnapshotter: Send + Sync {
    fn capture(
        &self,
        duration_secs: u64,
    ) -> Result<crate::proc_snapshot::ProcSnapshotResult, String>;
}

pub struct RealProcessSnapshotter;

impl ProcessSnapshotter for RealProcessSnapshotter {
    fn capture(
        &self,
        duration_secs: u64,
    ) -> Result<crate::proc_snapshot::ProcSnapshotResult, String> {
        crate::proc_snapshot::capture(duration_secs)
    }
}
