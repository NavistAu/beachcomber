//! Regression test for the daemon-leak bug.
//!
//! A `comb daemon` must not outlive the process that spawned it. The test
//! harness (`tests/common/daemon.rs`) cleans up in `Drop`, but `Drop` does not
//! run when nextest SIGKILLs a timed-out/hung test — which orphaned ~71 debug
//! daemons (each holding an FSEvents client and pinning `fseventsd` to ~2 GB)
//! during the test-suite-health work. `--exit-with-parent` makes the daemon
//! self-terminate when its spawner dies, even on SIGKILL.

use crate::common::daemon::{isolate, launch};
use crate::common::poll::poll_until;
use std::process::Command;
use std::time::Duration;

/// How often a test looks again: the daemon checks its parent every 50ms.
const POLL: Duration = Duration::from_millis(10);

/// True while `pid` is a live process: signal 0 checks existence without
/// sending anything.
fn pid_alive(pid: libc::pid_t) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

/// The pid `sh` recorded in `pidfile`.
fn read_pid(pidfile: &std::path::Path) -> libc::pid_t {
    std::fs::read_to_string(pidfile)
        .expect("read daemon pidfile")
        .trim()
        .parse()
        .expect("a pid")
}

/// Kills the daemon pid on drop, so a failed or timed-out test cannot leak it.
struct KillOnDrop(libc::pid_t);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        unsafe { libc::kill(self.0, libc::SIGKILL) };
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
    // the developer's real config directory. `launch` hands `sh` the readiness
    // pipe as `$0 $1` (`--ready-fd <fd>`), which `sh` passes on to the daemon
    // and closes, and returns once the daemon says it serves.
    let mut cmd = Command::new("sh");
    isolate(&mut cmd, tmp.path(), tmp.path(), &sock)
        .arg("-c")
        .arg(format!(
            "{comb} daemon --exit-with-parent --socket {sock} --ready-fd $1 & echo $! > {pid}; eval \"exec $1>&-\"; wait",
            sock = sock.display(),
            pid = pidfile.display(),
        ));
    let mut parent = launch(&mut cmd);
    let daemon_pid = read_pid(&pidfile);
    let _reaper = KillOnDrop(daemon_pid);
    assert!(
        pid_alive(daemon_pid),
        "daemon (pid {daemon_pid}) should be alive before the parent dies"
    );

    // Kill the parent with no chance to clean up.
    parent.kill().expect("kill intermediate parent");
    let _ = parent.wait();

    // The daemon must self-exit within a few poll intervals (it polls ppid @50ms).
    let still_alive = poll_until(
        &format!("liveness of daemon pid {daemon_pid}"),
        "it to self-exit after its parent died (--exit-with-parent)",
        Duration::from_secs(10),
        POLL,
        || pid_alive(daemon_pid),
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
    let daemon_pid = read_pid(&pidfile);
    let _reaper = KillOnDrop(daemon_pid);
    parent.kill().expect("kill intermediate parent");
    let _ = parent.wait();

    let still_alive = poll_until(
        &format!("liveness of daemon pid {daemon_pid}"),
        "it to self-exit after its parent died mid-startup (--exit-with-parent)",
        Duration::from_secs(10),
        POLL,
        || pid_alive(daemon_pid),
        |alive| !*alive,
    );
    assert!(
        !still_alive,
        "daemon (pid {daemon_pid}) must self-exit when its parent died during startup"
    );
}

/// The parent is already gone before `run_daemon` begins, so the daemon's
/// parent at startup is pid 1: there is no parent to follow and it must exit
/// at once. A gate file holds the daemon back until the parent has exited, so
/// the ordering is deterministic.
#[test]
fn daemon_exits_at_once_when_already_orphaned_at_startup() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let pidfile = tmp.path().join("daemon.pid");
    let gate = tmp.path().join("go");
    let comb = env!("CARGO_BIN_EXE_comb");

    let mut parent = isolate(&mut Command::new("sh"), tmp.path(), tmp.path(), &sock)
        .arg("-c")
        .arg(format!(
            "( while [ ! -e {gate} ]; do :; done; exec {comb} daemon --exit-with-parent --socket {sock} ) & echo $! > {pid}",
            gate = gate.display(),
            sock = sock.display(),
            pid = pidfile.display(),
        ))
        .spawn()
        .expect("spawn short-lived parent");
    let _ = parent.wait();

    let daemon_pid = read_pid(&pidfile);
    let _reaper = KillOnDrop(daemon_pid);

    // The parent has exited; release the daemon so it starts already orphaned.
    std::fs::write(&gate, "").expect("open gate");

    let still_alive = poll_until(
        &format!("liveness of daemon pid {daemon_pid}"),
        "it to exit at once, having no parent at startup (--exit-with-parent)",
        Duration::from_secs(10),
        POLL,
        || pid_alive(daemon_pid),
        |alive| !*alive,
    );
    assert!(
        !still_alive,
        "daemon (pid {daemon_pid}) must exit at once when orphaned at startup"
    );
}
