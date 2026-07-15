//! Pure decision logic for the singleton enforcement layer — no OS calls, fully unit-testable.
//!
//! Functions here operate only on their arguments and return plain values.
//! OS-bound work (filesystem access, process signaling) lives in
//! `crate::singleton` (`src/singleton/mod.rs`).

use crate::singleton::PidFileRecord;

/// Outcome of a supersession check.
///
/// See `decide_supersession` for the full decision rule.
pub use crate::singleton::SupersessionDecision;

/// Given the existing singleton's record, our own binary hash, and whether the
/// existing owner is serving its socket, decide whether to supersede (kill and
/// take over) or exit silently (existing daemon is fine).
///
/// Different hash → supersede. Same hash + serving → exit silently. Same hash but
/// not serving → supersede (owner wedged before bind, or socket deleted).
///
/// This is the same function re-exported from `crate::singleton` for callers that
/// want to import from the pure-policy module explicitly.
pub fn decide_supersession(
    existing: &PidFileRecord,
    our_binary_hash: &str,
    owner_serving: bool,
) -> SupersessionDecision {
    crate::singleton::decide_supersession(existing, our_binary_hash, owner_serving)
}

/// Given a file's last-modified timestamp (in milliseconds since the Unix epoch) and
/// the process-start timestamp (also in milliseconds since the Unix epoch), decide
/// whether the binary is strictly newer than the process start time.
///
/// Returns `true` when the file was modified *after* the process started, meaning the
/// on-disk binary has changed and the daemon should restart.
///
/// This function is the pure comparison half of `binary_newer_than`; the OS-bound half
/// (reading the file's mtime via `std::fs::metadata`) stays in `crate::singleton`.
pub fn is_binary_newer(mtime_unix_ms: u64, process_start_unix_ms: u64) -> bool {
    mtime_unix_ms > process_start_unix_ms
}

/// Context the reaper carries into each per-candidate decision.
///
/// See `docs/canon/singleton.md` §"Orphan reaping".
pub struct ReapContext {
    pub our_pid: u32,
    pub our_socket: std::path::PathBuf,
    pub grace_age_secs: u64,
}

/// Per-candidate reap decision. `Exempt` carries the matched rule for logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReapDecision {
    Reap,
    Exempt(&'static str),
}

/// True when `argv` describes a `comb daemon` invocation (`daemon` or its
/// visible alias `d`).
pub fn is_comb_daemon_argv(argv: &[String]) -> bool {
    let Some(arg0) = argv.first() else {
        return false;
    };
    let bin = std::path::Path::new(arg0)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if bin != "comb" {
        return false;
    }
    matches!(argv.get(1).map(String::as_str), Some("daemon") | Some("d"))
}

/// Extract the `--socket` value from argv (`--socket <path>` or `--socket=<path>`).
pub fn socket_arg(argv: &[String]) -> Option<std::path::PathBuf> {
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        if a == "--socket" {
            return it.next().map(std::path::PathBuf::from);
        }
        if let Some(v) = a.strip_prefix("--socket=") {
            return Some(std::path::PathBuf::from(v));
        }
    }
    None
}

/// The canon reap rule: uid-owned `comb daemon` processes are orphans unless
/// an exemption applies. Exemptions, in canon order:
///
/// 1. the reaper itself, or a process on the reaper's own socket path
///    (startup contention — flock and serving probe govern those);
/// 2. `--exit-with-parent` — self-cleaning test daemons;
/// 3. `--no-reap` — explicit opt-out for supervised side daemons;
/// 4. still parented (PPID ≠ 1) — attended foreground runs;
/// 5. younger than the grace age — never race a daemon mid-startup.
///
/// Pure: uid filtering happens in the `ProcessTable` boundary.
pub fn decide_reap(
    candidate: &crate::boundaries::proc_table::ProcessInfo,
    ctx: &ReapContext,
) -> ReapDecision {
    if !is_comb_daemon_argv(&candidate.argv) {
        return ReapDecision::Exempt("not a comb daemon");
    }
    if candidate.pid == ctx.our_pid {
        return ReapDecision::Exempt("self");
    }
    if socket_arg(&candidate.argv).is_some_and(|s| s == ctx.our_socket) {
        return ReapDecision::Exempt("same socket path");
    }
    if candidate.argv.iter().any(|a| a == "--exit-with-parent") {
        return ReapDecision::Exempt("exit-with-parent");
    }
    if candidate.argv.iter().any(|a| a == "--no-reap") {
        return ReapDecision::Exempt("no-reap");
    }
    if candidate.ppid != 1 {
        return ReapDecision::Exempt("attended (parent alive)");
    }
    if candidate.age_secs < ctx.grace_age_secs {
        return ReapDecision::Exempt("younger than grace age");
    }
    ReapDecision::Reap
}

/// True when a daemon bound to `bound_socket` is the canonical daemon: its
/// bound socket equals its own env-free canonical resolution. Only the
/// canonical daemon reaps.
pub fn is_canonical_daemon(
    bound_socket: &std::path::Path,
    resolved_canonical: &std::path::Path,
) -> bool {
    bound_socket == resolved_canonical
}

/// Decide whether a pid-file record describes a stale entry that can be reclaimed.
///
/// A record is considered stale (and safe to reclaim) when the process is **not**
/// running (`process_alive` = false).  When the process is alive the record is fresh
/// regardless of any other field.
///
/// The OS-bound caller is responsible for checking liveness (e.g., `kill(pid, 0)`)
/// and supplying the boolean.
pub fn is_pidfile_stale(process_alive: bool) -> bool {
    !process_alive
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::singleton::{PidFileRecord, SupersessionDecision};

    const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn make_record(pid: u32, hash: &str) -> PidFileRecord {
        PidFileRecord {
            pid,
            version: "0.5.1".into(),
            binary: "/path/to/comb".into(),
            binary_hash: hash.into(),
            started_unix_ms: 0,
        }
    }

    // --- decide_supersession ---

    #[test]
    fn same_hash_serving_means_exit_silent() {
        let rec = make_record(1234, HASH_A);
        let decision = decide_supersession(&rec, HASH_A, true);
        assert!(
            matches!(decision, SupersessionDecision::ExitSilent),
            "same hash + serving should yield ExitSilent, got {decision:?}"
        );
    }

    #[test]
    fn same_hash_not_serving_means_supersede() {
        let rec = make_record(1234, HASH_A);
        let decision = decide_supersession(&rec, HASH_A, false);
        match decision {
            SupersessionDecision::Supersede { existing_pid } => {
                assert_eq!(existing_pid, 1234, "should carry the existing PID");
            }
            other => panic!("expected Supersede for non-serving owner, got {other:?}"),
        }
    }

    #[test]
    fn different_hash_means_supersede() {
        let rec = make_record(1234, HASH_A);
        // Serving state is irrelevant when the build differs.
        let decision = decide_supersession(&rec, HASH_B, true);
        match decision {
            SupersessionDecision::Supersede { existing_pid } => {
                assert_eq!(existing_pid, 1234, "should carry the existing PID");
            }
            other => panic!("expected Supersede, got {other:?}"),
        }
    }

    #[test]
    fn same_version_different_hash_still_supersedes() {
        // Two dev builds at the same cargo version but different binaries —
        // human version matches, but binary_hash differs, so we supersede.
        let mut rec = make_record(9999, HASH_A);
        rec.version = "0.5.1".into(); // same version string
        let decision = decide_supersession(&rec, HASH_B, true);
        assert!(
            matches!(decision, SupersessionDecision::Supersede { .. }),
            "version string is advisory only; hash difference must supersede"
        );
    }

    #[test]
    fn supersede_carries_existing_pid_correctly() {
        let rec = make_record(42, HASH_A);
        let decision = decide_supersession(&rec, HASH_B, true);
        if let SupersessionDecision::Supersede { existing_pid } = decision {
            assert_eq!(existing_pid, 42);
        } else {
            panic!("expected Supersede");
        }
    }

    // --- is_binary_newer ---

    #[test]
    fn binary_newer_when_mtime_after_start() {
        assert!(is_binary_newer(1000, 999));
    }

    #[test]
    fn binary_not_newer_when_mtime_equals_start() {
        assert!(!is_binary_newer(1000, 1000));
    }

    #[test]
    fn binary_not_newer_when_mtime_before_start() {
        assert!(!is_binary_newer(999, 1000));
    }

    #[test]
    fn binary_newer_handles_zero_start() {
        // Any reasonable mtime is newer than epoch=0.
        assert!(is_binary_newer(1, 0));
    }

    #[test]
    fn binary_not_newer_at_exact_epoch() {
        assert!(!is_binary_newer(0, 0));
    }

    // --- is_pidfile_stale ---

    #[test]
    fn stale_when_process_gone() {
        assert!(is_pidfile_stale(false));
    }

    #[test]
    fn fresh_when_process_alive() {
        assert!(!is_pidfile_stale(true));
    }
}
