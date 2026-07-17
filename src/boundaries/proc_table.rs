//! ProcessTable boundary trait — abstracts uid-owned process enumeration from OS state.
//!
//! Used by the singleton's orphan reaping (see `docs/canon/singleton.md`
//! §"Orphan reaping"). Hand-rolled per platform (`sysctl KERN_PROC_ALL` on
//! macOS, /proc on Linux) — deliberately no external dependency.
//!
//! macOS must NOT use libproc's `proc_listallpids` for the pid list: seatbelt
//! sandbox profiles filter it to the session's own processes (observed: 51 of
//! 737 system pids) while leaving `sysctl KERN_PROC_ALL` unrestricted, which
//! made a sandbox-spawned canonical daemon silently blind to every orphan it
//! existed to reap. Per-pid detail calls (`proc_pidinfo`, `KERN_PROCARGS2`)
//! are not filtered and stay as-is. See canon §"Reaper visibility self-test".

/// One row of the process table, as much as reaping needs to know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub ppid: u32,
    /// Full argument vector; argv[0] is the executable as invoked.
    pub argv: Vec<String>,
    /// Seconds since the process started.
    pub age_secs: u64,
}

#[cfg_attr(test, mockall::automock)]
pub trait ProcessTable: Send + Sync {
    /// Processes owned by the current real uid. Entries whose metadata or argv
    /// cannot be read (permission, zombie, raced exit) are omitted.
    fn list_own(&self) -> Vec<ProcessInfo>;

    /// Reaper visibility self-test (canon singleton.md): true when PID 1
    /// appears in the RAW enumeration (before uid filtering). False means the
    /// process table is confined (sandbox, hidepid) and reaping is blind to
    /// processes outside the confinement.
    fn pid1_visible(&self) -> bool;
}

pub struct RealProcessTable;

impl ProcessTable for RealProcessTable {
    fn list_own(&self) -> Vec<ProcessInfo> {
        imp::list_own()
    }

    fn pid1_visible(&self) -> bool {
        imp::pid1_visible()
    }
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{ProcessInfo, now_unix_secs};
    use std::ffi::c_void;

    /// `struct kinfo_proc` ABI facts, verified against `<sys/sysctl.h>` via
    /// `sizeof`/`offsetof` on arm64 and x86_64 (identical; the layout is
    /// frozen public ABI). Only the pid is read from the sysctl records —
    /// per-pid detail still comes from `proc_pidinfo`, which sandboxes do not
    /// filter. `raw_pids_includes_self` below is the runtime canary for these
    /// constants: if they were wrong, our own pid would not be found.
    const KINFO_PROC_SIZE: usize = 648;
    const KINFO_PROC_PID_OFFSET: usize = 40;

    /// System-wide pid list via `sysctl KERN_PROC_ALL`.
    ///
    /// NOT `proc_listallpids`: seatbelt sandbox profiles filter libproc's pid
    /// list to the session while leaving this sysctl unrestricted (canon
    /// singleton.md §"Reaper visibility self-test", 2026-07-16 incident).
    fn raw_pids() -> Vec<libc::c_int> {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_ALL, 0];

        // First call sizes the buffer; pad for processes spawned between the
        // two calls, then the second call fills it.
        let mut len: libc::size_t = 0;
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                4,
                std::ptr::null_mut(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || len == 0 {
            return Vec::new();
        }
        len += 64 * KINFO_PROC_SIZE;
        let mut buf = vec![0u8; len];
        let mut got = len;
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                4,
                buf.as_mut_ptr() as *mut c_void,
                &mut got,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return Vec::new();
        }

        (0..got / KINFO_PROC_SIZE)
            .filter_map(|i| {
                let at = i * KINFO_PROC_SIZE + KINFO_PROC_PID_OFFSET;
                let bytes: [u8; 4] = buf.get(at..at + 4)?.try_into().ok()?;
                Some(libc::c_int::from_ne_bytes(bytes))
            })
            .collect()
    }

    pub fn pid1_visible() -> bool {
        raw_pids().contains(&1)
    }

    pub fn list_own() -> Vec<ProcessInfo> {
        let our_uid = unsafe { libc::getuid() };
        let now = now_unix_secs();

        raw_pids()
            .into_iter()
            .filter(|&pid| pid > 0)
            .filter_map(|pid| {
                let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
                let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
                let n = unsafe {
                    libc::proc_pidinfo(
                        pid,
                        libc::PROC_PIDTBSDINFO,
                        0,
                        &mut info as *mut _ as *mut c_void,
                        size,
                    )
                };
                if n != size || info.pbi_uid != our_uid {
                    return None;
                }
                let argv = kern_procargs2(pid)?;
                Some(ProcessInfo {
                    pid: pid as u32,
                    ppid: info.pbi_ppid,
                    argv,
                    age_secs: now.saturating_sub(info.pbi_start_tvsec),
                })
            })
            .collect()
    }

    /// Read a process's argv via `sysctl KERN_PROCARGS2`.
    ///
    /// Buffer layout: `i32 argc`, then the exec path (NUL-terminated), NUL
    /// padding, then `argc` NUL-terminated argv strings (env vars follow; not
    /// read). Fails (None) for zombies and processes we lack rights to inspect.
    fn kern_procargs2(pid: libc::c_int) -> Option<Vec<String>> {
        let mut argmax: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>();
        let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                2,
                &mut argmax as *mut _ as *mut c_void,
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || argmax <= 0 {
            return None;
        }

        let mut buf = vec![0u8; argmax as usize];
        let mut size = buf.len();
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buf.as_mut_ptr() as *mut c_void,
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || size < 4 {
            return None;
        }
        buf.truncate(size);

        let argc = i32::from_ne_bytes(buf[0..4].try_into().ok()?);
        if argc <= 0 {
            return None;
        }

        // Skip the exec path and the NUL padding that follows it.
        let mut i = 4;
        while i < buf.len() && buf[i] != 0 {
            i += 1;
        }
        while i < buf.len() && buf[i] == 0 {
            i += 1;
        }

        let mut argv = Vec::with_capacity(argc as usize);
        for _ in 0..argc {
            let start = i;
            while i < buf.len() && buf[i] != 0 {
                i += 1;
            }
            if start >= buf.len() {
                break;
            }
            argv.push(String::from_utf8_lossy(&buf[start..i]).into_owned());
            i += 1; // step over the NUL
        }
        if argv.is_empty() { None } else { Some(argv) }
    }

    #[cfg(test)]
    mod tests {
        /// Runtime canary for `KINFO_PROC_SIZE`/`KINFO_PROC_PID_OFFSET`: if
        /// either constant drifted from the real `kinfo_proc` layout, pid
        /// extraction would yield garbage and our own pid would be absent.
        #[test]
        fn raw_pids_includes_self() {
            let me = std::process::id() as libc::c_int;
            assert!(
                super::raw_pids().contains(&me),
                "own pid missing from KERN_PROC_ALL walk — kinfo_proc ABI constants wrong?"
            );
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{ProcessInfo, now_unix_secs};
    use std::os::unix::fs::MetadataExt;

    /// Reaper visibility probe: /proc/1 is present in any healthy procfs view
    /// (in a PID namespace it is the namespace's own init, which is correct —
    /// that namespace is the reaper's jurisdiction). `hidepid=2` mounts hide
    /// it from non-owners — genuinely confined, so degraded is the right call.
    pub fn pid1_visible() -> bool {
        std::path::Path::new("/proc/1").exists()
    }

    pub fn list_own() -> Vec<ProcessInfo> {
        let our_uid = unsafe { libc::getuid() };
        let ticks_per_sec = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
        let Some(boot_unix_secs) = boot_time_unix_secs() else {
            return Vec::new();
        };
        let now = now_unix_secs();

        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|e| {
                let pid: u32 = e.file_name().to_str()?.parse().ok()?;
                let meta = e.metadata().ok()?;
                if meta.uid() != our_uid {
                    return None;
                }
                let stat = std::fs::read_to_string(e.path().join("stat")).ok()?;
                // Fields after the comm, which is parenthesised and may contain
                // spaces: split on the LAST ')' and index from there.
                let after_comm = &stat[stat.rfind(')')? + 2..];
                let fields: Vec<&str> = after_comm.split_whitespace().collect();
                let ppid: u32 = fields.first()?.parse().ok()?; // field 4 overall
                let starttime_ticks: u64 = fields.get(19)?.parse().ok()?; // field 22 overall
                let started_unix = boot_unix_secs + starttime_ticks / ticks_per_sec;

                let cmdline = std::fs::read(e.path().join("cmdline")).ok()?;
                let argv: Vec<String> = cmdline
                    .split(|&b| b == 0)
                    .filter(|s| !s.is_empty())
                    .map(|s| String::from_utf8_lossy(s).into_owned())
                    .collect();
                if argv.is_empty() {
                    return None; // kernel thread or zombie
                }
                Some(ProcessInfo {
                    pid,
                    ppid,
                    argv,
                    age_secs: now.saturating_sub(started_unix),
                })
            })
            .collect()
    }

    fn boot_time_unix_secs() -> Option<u64> {
        let stat = std::fs::read_to_string("/proc/stat").ok()?;
        stat.lines()
            .find(|l| l.starts_with("btime "))?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()
    }
}
