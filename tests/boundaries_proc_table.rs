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
