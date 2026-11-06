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

#[cfg(test)]
mod tests {
    use super::{ProcessSnapshotter, RealProcessSnapshotter};

    #[test]
    fn real_snapshotter_implements_trait() {
        // Confirm RealProcessSnapshotter satisfies the ProcessSnapshotter trait bound.
        fn assert_impl<T: ProcessSnapshotter>(_: &T) {}
        let s = RealProcessSnapshotter;
        assert_impl(&s);
    }

    #[test]
    fn capture_zero_seconds_does_not_panic() {
        // duration_secs = 0: on all platforms the sampling window is empty so capture
        // returns Err (no events). That is the expected graceful outcome; the test
        // just confirms it doesn't panic.
        let s = RealProcessSnapshotter;
        let _ = s.capture(0);
    }
}
