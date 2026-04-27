mod common;
use common::daemon::TestDaemon;

#[test]
fn daemon_fixture_starts_and_exits() {
    let d = TestDaemon::spawn();
    assert!(d.socket.path.exists());
    drop(d);
}
