//! Regression test for the daemon-leak bug.
//!
//! A `comb daemon` must not outlive the process that spawned it. The test
//! harness (`tests/common/daemon.rs`) cleans up in `Drop`, but `Drop` does not
//! run when nextest SIGKILLs a timed-out/hung test — which orphaned ~71 debug
//! daemons (each holding an FSEvents client and pinning `fseventsd` to ~2 GB)
//! during the test-suite-health work. `--exit-with-parent` makes the daemon
//! self-terminate when its spawner dies, even on SIGKILL.

use crate::common::daemon::isolate;
use crate::common::poll::poll_until;
use std::process::Command;
use std::time::Duration;

/// True while `pid` (as a decimal string) is a live process. `kill -0` checks
/// existence without sending a real signal; avoids a libc dev-dependency.
fn pid_alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Kills the daemon pid on drop, so a failed or timed-out test cannot leak it.
struct KillOnDrop(String);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = Command::new("kill").args(["-9", &self.0]).status();
    }
}

#[test]
fn daemon_exits_when_parent_dies() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let pidfile = tmp.path().join("daemon.pid");
    let comb = env!("CARGO_BIN_EXE_comb");

    // Intermediate `sh` is the daemon's parent: it backgrounds the daemon (so the
    // daemon is its child), records the daemon's pid, then blocks in `wait`.
    // SIGKILLing this `sh` re-parents the daemon WITHOUT signalling it — exactly
    // the nextest-SIGKILL-of-a-test scenario that produced the orphans.
    // `HOME`/`XDG_CONFIG_HOME` keep the daemon's managed-defaults write out of
    // the developer's real config directory.
    let mut parent = isolate(&mut Command::new("sh"), tmp.path(), tmp.path(), &sock)
        .arg("-c")
        .arg(format!(
            "{comb} daemon --exit-with-parent --socket {sock} & echo $! > {pid}; wait",
            sock = sock.display(),
            pid = pidfile.display(),
        ))
        .spawn()
        .expect("spawn intermediate parent");

    // Wait for the daemon to come up (socket + recorded pid).
    poll_until(
        &format!("(socket at {sock:?}, pidfile at {pidfile:?})"),
        "both to exist",
        Duration::from_secs(10),
        Duration::from_millis(50),
        || (sock.exists(), pidfile.exists()),
        |(has_sock, has_pid)| *has_sock && *has_pid,
    );
    let daemon_pid = std::fs::read_to_string(&pidfile)
        .expect("read daemon pidfile")
        .trim()
        .to_string();
    let _reaper = KillOnDrop(daemon_pid.clone());
    assert!(
        pid_alive(&daemon_pid),
        "daemon (pid {daemon_pid}) should be alive before the parent dies"
    );

    // Kill the parent with no chance to clean up.
    parent.kill().expect("kill intermediate parent");
    let _ = parent.wait();

    // The daemon must self-exit within a few poll intervals (it polls ppid @500ms).
    let still_alive = poll_until(
        &format!("liveness of daemon pid {daemon_pid}"),
        "it to self-exit after its parent died (--exit-with-parent)",
        Duration::from_secs(10),
        Duration::from_millis(100),
        || pid_alive(&daemon_pid),
        |alive| !*alive,
    );
    assert!(
        !still_alive,
        "daemon (pid {daemon_pid}) must self-exit after its parent died (--exit-with-parent)"
    );
}

/// The parent dies while the daemon is still starting up (its log exists, its
/// socket does not). The daemon must still notice: its parent is captured
/// before any startup work, not when the watch is armed.
#[test]
fn daemon_exits_when_parent_dies_during_startup() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let pidfile = tmp.path().join("daemon.pid");
    let log = tmp.path().join(".local/state/beachcomber/daemon.log");
    let comb = env!("CARGO_BIN_EXE_comb");

    let mut parent = isolate(&mut Command::new("sh"), tmp.path(), tmp.path(), &sock)
        .arg("-c")
        .arg(format!(
            "{comb} daemon --exit-with-parent --socket {sock} & echo $! > {pid}; wait",
            sock = sock.display(),
            pid = pidfile.display(),
        ))
        .spawn()
        .expect("spawn intermediate parent");

    // The log is opened after the daemon has started running, before its socket
    // binds: the earliest observable point of startup. Kill the parent there.
    poll_until(
        &format!("(log at {log:?}, pidfile at {pidfile:?})"),
        "both to exist",
        Duration::from_secs(10),
        Duration::from_millis(1),
        || (log.exists(), pidfile.exists()),
        |(has_log, has_pid)| *has_log && *has_pid,
    );
    let daemon_pid = std::fs::read_to_string(&pidfile)
        .expect("read daemon pidfile")
        .trim()
        .to_string();
    let _reaper = KillOnDrop(daemon_pid.clone());
    parent.kill().expect("kill intermediate parent");
    let _ = parent.wait();

    let still_alive = poll_until(
        &format!("liveness of daemon pid {daemon_pid}"),
        "it to self-exit after its parent died mid-startup (--exit-with-parent)",
        Duration::from_secs(10),
        Duration::from_millis(100),
        || pid_alive(&daemon_pid),
        |alive| !*alive,
    );
    assert!(
        !still_alive,
        "daemon (pid {daemon_pid}) must self-exit when its parent died during startup"
    );
}
