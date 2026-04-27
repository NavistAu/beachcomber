use beachcomber::boundaries::proc_snap::{ProcessSnapshotter, RealProcessSnapshotter};

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
