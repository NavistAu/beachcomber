//! PID validation for `comb kill` fallback. Separates the branching logic from
//! the platform-specific `comm` reads so the logic can be unit-tested.

/// Decide whether a PID identifies the beachcomber daemon given:
/// - whether `kill(pid, 0)` succeeded (i.e., the process exists and is signalable by us)
/// - the process's `comm` (name), if we were able to read it
///
/// The process is accepted iff the kill check passed AND the comm trimmed equals `comb`.
pub fn pid_matches_our_daemon(kill0_ok: bool, comm: Option<String>) -> bool {
    if !kill0_ok {
        return false;
    }
    match comm {
        Some(s) => s.trim() == "comb",
        None => false,
    }
}

/// Return true if `pid` exists, is signalable, and its comm matches `comb`.
/// Combines the `kill(pid, 0)` probe with the comm read so callers need only
/// one import.
pub fn pid_is_our_daemon(pid: i32) -> bool {
    let kill0_ok = unsafe { libc::kill(pid, 0) == 0 };
    let comm = read_process_comm(pid);
    pid_matches_our_daemon(kill0_ok, comm)
}

/// Read the process name (argv[0] / executable comm) for `pid`.
/// Returns None on any read or platform failure.
pub(crate) fn read_process_comm(pid: i32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("ps")
            .args(["-o", "comm=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if text.is_empty() {
            return None;
        }
        // macOS ps reports the full path; take the basename.
        Some(
            std::path::Path::new(&text)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or(text),
        )
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}
