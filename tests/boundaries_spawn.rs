use beachcomber::boundaries::spawn::{DaemonSpawner, RealDaemonSpawner};

#[test]
fn real_daemon_spawner_implements_trait() {
    // Confirm RealDaemonSpawner satisfies the DaemonSpawner trait bound at compile time.
    // No actual daemon is spawned here; that requires an isolated environment.
    fn assert_impl<T: DaemonSpawner>(_: &T) {}
    let s = RealDaemonSpawner;
    assert_impl(&s);
}
