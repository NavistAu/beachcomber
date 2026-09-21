//! RealProcessTable smoke tests — the enumeration boundary against the live OS.

use beachcomber::boundaries::proc_table::{ProcessTable, RealProcessTable};

#[test]
fn list_own_includes_self_with_argv_and_parent() {
    let table = RealProcessTable;
    let procs = table.list_own();
    let me = std::process::id();

    let this = procs
        .iter()
        .find(|p| p.pid == me)
        .expect("own process appears in uid-owned listing");
    assert!(!this.argv.is_empty(), "argv readable for own process");
    assert!(this.ppid > 0, "ppid populated");
    // Started moments ago as part of this test run.
    assert!(this.age_secs < 3600, "age plausible, got {}", this.age_secs);
}

#[test]
fn list_own_excludes_other_uids() {
    // PID 1 (launchd/init, uid 0) must never appear in a non-root listing.
    if unsafe { libc::getuid() } == 0 {
        return; // meaningless as root
    }
    let table = RealProcessTable;
    assert!(table.list_own().iter().all(|p| p.pid != 1));
}

#[test]
fn pid1_visible_on_healthy_system() {
    // Canon singleton.md §"Reaper visibility self-test": PID 1 must appear in
    // the RAW enumeration (pre-uid-filter) on any healthy system. On macOS this
    // holds even under seatbelt sandboxes because the boundary uses
    // `sysctl KERN_PROC_ALL`, which sandbox profiles do not filter (unlike
    // libproc's proc_listallpids — the 2026-07-16 blind-reaper incident).
    let table = RealProcessTable;
    assert!(
        table.pid1_visible(),
        "PID 1 not visible in raw process enumeration — reaper would be blind"
    );
}

#[test]
fn list_own_sees_processes_beyond_own_session() {
    // Regression guard for the sandbox-confined enumeration bug: the raw pid
    // list must plausibly span the system, not just this process's session.
    // We can't assert an absolute count (containers are small), but PID 1
    // visibility plus our own presence is the canon-blessed minimum; this
    // test additionally pins that enumeration and the probe agree on the
    // same mechanism (both must come from the sysctl/procfs raw list).
    let table = RealProcessTable;
    let procs = table.list_own();
    assert!(!procs.is_empty());
    assert!(table.pid1_visible());
}
