//! Singleton daemon enforcement: PID file with flock, build-identity comparison,
//! graceful handover on build mismatch, same-build serving probe.

pub mod policy;

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct PidFileRecord {
    pub pid: u32,
    /// Human-readable version string (e.g. `0.5.1` or `0.5.1+sha.abc12345`).
    /// Advisory only — NOT used for supersession identity. Two dev builds at the
    /// same cargo version both show `0.5.1` here but will differ in `binary_id`.
    pub version: String,
    pub binary: PathBuf,
    /// [`binary_id`] of the binary at daemon startup. This IS the build
    /// identity used by `decide_supersession`. Same id = same build = no-op;
    /// different id = different build = supersede. Older daemons wrote a
    /// SHA-256 here as `binary_id`; it never matches an id, so they supersede.
    #[serde(alias = "binary_hash")]
    pub binary_id: String,
    pub started_unix_ms: u64,
}

/// The build identity of `current_exe()`: [`binary_id`] of its path.
pub fn current_binary_id() -> std::io::Result<String> {
    binary_id(&std::env::current_exe()?)
}

/// A build identity for the file at `path`, from its metadata alone: device,
/// inode, size and modification time. A rebuild or reinstall writes a new
/// file, so its identity changes; reading it costs one `stat`, where hashing
/// the content cost tens of milliseconds on every daemon start.
pub fn binary_id(path: &Path) -> std::io::Result<String> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path)?;
    Ok(format!(
        "{:x}-{:x}-{:x}-{}.{:09}",
        m.dev(),
        m.ino(),
        m.size(),
        m.mtime(),
        m.mtime_nsec()
    ))
}

#[derive(Debug, thiserror::Error)]
pub enum SingletonLockError {
    #[error("flock contended — another daemon holds the lock: {existing:?}")]
    AlreadyHeld { existing: Option<PidFileRecord> },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub struct SingletonLock {
    pid_path: PathBuf,
    _file: File,
}

impl SingletonLock {
    pub fn acquire(
        pid_path: &Path,
        version: &str,
        binary_id: &str,
    ) -> Result<Self, SingletonLockError> {
        if let Some(parent) = pid_path.parent() {
            std::fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(parent)?.permissions();
                perms.set_mode(0o700);
                std::fs::set_permissions(parent, perms)?;
            }
        }

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(pid_path)?;

        let fd = file.as_raw_fd();
        let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            let errno = std::io::Error::last_os_error();
            if errno.raw_os_error() == Some(libc::EWOULDBLOCK) {
                let existing = Self::read_record(pid_path).ok();
                return Err(SingletonLockError::AlreadyHeld { existing });
            }
            return Err(SingletonLockError::Io(errno));
        }

        let record = PidFileRecord {
            pid: std::process::id(),
            version: version.into(),
            binary: std::env::current_exe().unwrap_or_default(),
            binary_id: binary_id.into(),
            started_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        };
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(serde_json::to_string(&record)?.as_bytes())?;
        file.flush()?;

        Ok(Self {
            pid_path: pid_path.to_path_buf(),
            _file: file,
        })
    }

    pub fn read_record(pid_path: &Path) -> Result<PidFileRecord, SingletonLockError> {
        let mut file = File::open(pid_path)?;
        let mut s = String::new();
        file.read_to_string(&mut s)?;
        Ok(serde_json::from_str(&s)?)
    }
}

impl Drop for SingletonLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.pid_path);
    }
}

#[derive(Debug)]
pub enum SupersessionDecision {
    /// Existing daemon has the same version; new daemon should exit silently.
    ExitSilent,
    /// Existing daemon is a different build; new daemon should kill it and take over.
    Supersede { existing_pid: u32 },
}

/// Given the existing singleton's record, our own binary id, and whether the
/// existing owner is actually serving its socket, decide whether to supersede
/// (kill and take over) or exit silently (existing daemon is fine).
///
/// Compares on `binary_id` — the identity of the binary file at daemon
/// startup — and the owner's serving state:
/// - Different id → supersede (rebuilt binary), regardless of serving state.
/// - Same id **and serving** → exit silently; the existing daemon is healthy.
/// - Same id but **not serving** → supersede; the owner is wedged between flock
///   and bind (or its socket was deleted), so a healthy daemon must rebind.
///
/// See `website/docs/internals/architecture.md` §7.
pub fn decide_supersession(
    existing: &PidFileRecord,
    our_binary_id: &str,
    owner_serving: bool,
) -> SupersessionDecision {
    if existing.binary_id == our_binary_id && owner_serving {
        SupersessionDecision::ExitSilent
    } else {
        SupersessionDecision::Supersede {
            existing_pid: existing.pid,
        }
    }
}

/// Probe `socket` until a daemon is serving it or `grace` elapses.
///
/// Probes immediately (fast path: a serving owner returns `true` at once,
/// preserving the common idempotent-contention case). If the first probe fails,
/// retries every `step` ([`PROBE_STEP`] for a daemon) until the owner starts
/// serving or the grace window expires. A grace of `Duration::ZERO` probes
/// exactly once.
///
/// The grace window preserves the concurrent-start race (singleton.md Example 2):
/// the losing process waits for the winner to bind rather than killing a daemon
/// that is merely slow to reach `bind`.
pub fn probe_until_serving(
    probe: &dyn crate::boundaries::socket::SocketProbe,
    socket: &Path,
    grace: Duration,
    step: Duration,
) -> bool {
    let deadline = Instant::now() + grace;
    loop {
        if probe.is_serving(socket) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(step);
    }
}

/// How often a starting daemon probes a same-build owner's socket through
/// the grace window ([`probe_until_serving`]).
pub const PROBE_STEP: Duration = Duration::from_millis(50);

/// High-level: acquire the singleton lock, superseding an existing daemon if its
/// version differs from ours.
///
/// Returns:
/// - `Ok(Some(lock))`: we hold the singleton; run the daemon.
/// - `Ok(None)`: existing daemon has the same version; caller should exit silently.
/// - `Err(...)`: unexpected failure (IO, permission, etc.).
pub fn acquire_or_supersede(
    pid_path: &Path,
    socket_path: &Path,
    our_version: &str,
    our_binary_id: &str,
) -> Result<Option<SingletonLock>, SingletonLockError> {
    acquire_or_supersede_with(
        &crate::boundaries::socket::RealSocketProbe,
        Duration::from_secs(2),
        pid_path,
        socket_path,
        our_version,
        our_binary_id,
    )
}

/// Injection-point variant of [`acquire_or_supersede`] — accepts a [`SocketProbe`]
/// double and an explicit probe grace, for tests.
///
/// [`SocketProbe`]: crate::boundaries::socket::SocketProbe
pub fn acquire_or_supersede_with(
    probe: &dyn crate::boundaries::socket::SocketProbe,
    probe_grace: Duration,
    pid_path: &Path,
    socket_path: &Path,
    our_version: &str,
    our_binary_id: &str,
) -> Result<Option<SingletonLock>, SingletonLockError> {
    match SingletonLock::acquire(pid_path, our_version, our_binary_id) {
        Ok(lock) => Ok(Some(lock)),
        Err(SingletonLockError::AlreadyHeld {
            existing: Some(rec),
        }) => {
            // Same-build contention: probe the socket before concluding the owner
            // is fine. A different-build owner is superseded regardless, so skip
            // the (up-to-`probe_grace`) probe in that case.
            let owner_serving = if rec.binary_id == our_binary_id {
                probe_until_serving(probe, socket_path, probe_grace, PROBE_STEP)
            } else {
                false
            };
            match decide_supersession(&rec, our_binary_id, owner_serving) {
                SupersessionDecision::ExitSilent => Ok(None),
                SupersessionDecision::Supersede { existing_pid } => {
                    supersede_existing(existing_pid, Duration::from_secs(1))?;
                    // Retry acquire briefly; old daemon may take a moment to release.
                    let deadline = Instant::now() + Duration::from_secs(2);
                    loop {
                        match SingletonLock::acquire(pid_path, our_version, our_binary_id) {
                            Ok(lock) => return Ok(Some(lock)),
                            Err(SingletonLockError::AlreadyHeld { .. })
                                if Instant::now() < deadline =>
                            {
                                std::thread::sleep(Duration::from_millis(50));
                            }
                            Err(e) => return Err(e),
                        }
                    }
                }
            }
        }
        Err(SingletonLockError::AlreadyHeld { existing: None }) => {
            // PID file present but malformed and flock held — race or corruption.
            Err(SingletonLockError::AlreadyHeld { existing: None })
        }
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// Orphan reaping — canonical daemon only. See
// `website/docs/internals/architecture.md` §7.
// ---------------------------------------------------------------------------

/// Grace age below which a candidate is never reaped.
pub const REAP_GRACE_AGE_SECS: u64 = 60;

/// Outcome of one reap sweep. Feeds the per-sweep summary log
/// and the reaper health counters: a sweep
/// that found nothing eligible must be distinguishable from a sweep that
/// could not see anything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Uid-owned rows the process table returned.
    pub rows_enumerated: usize,
    /// Rows matching `comb daemon` argv (exemption rules apply to these).
    pub candidates: usize,
    /// Exemption tallies by matched rule, insertion-ordered.
    pub exemptions: Vec<(&'static str, usize)>,
    /// Pids successfully reaped.
    pub reaped: Vec<u32>,
    /// Kill attempts denied by the OS (EPERM — degraded signal capability).
    pub kill_denied: u32,
    /// Kill attempts failing for any other reason.
    pub kill_failed: u32,
    /// Reaper visibility self-test at this sweep: PID 1 present in the raw
    /// enumeration.
    pub pid1_visible: bool,
    /// Reaped orphans whose corpse files (socket + pid files) were removed.
    pub corpses_unlinked: u32,
}

impl SweepReport {
    fn tally(&mut self, rule: &'static str) {
        match self.exemptions.iter_mut().find(|(r, _)| *r == rule) {
            Some((_, n)) => *n += 1,
            None => self.exemptions.push((rule, 1)),
        }
    }
}

/// Shared reaper health state — written by the canonical daemon's reap loop,
/// read by the server's introspect handler. Backs the rule that reaper
/// capability is never silently degraded: the
/// visibility self-test verdict and OS-denied kill counts are observable via
/// `comb check daemon`, `comb status`, and introspect `reaper`.
#[derive(Debug, Default)]
pub struct ReaperHealth {
    /// True once the reap loop is armed (canonical daemon only). Side daemons
    /// and embedded/test servers never arm; their introspect reports reflect that.
    pub armed: std::sync::atomic::AtomicBool,
    /// Most recent visibility self-test verdict (PID 1 present in raw
    /// enumeration). Only meaningful once `armed`.
    pub visibility_ok: std::sync::atomic::AtomicBool,
    pub sweeps_total: std::sync::atomic::AtomicU64,
    pub reaped_total: std::sync::atomic::AtomicU64,
    /// Reap kills denied by the OS (EPERM) — degraded signal capability.
    pub kill_denied_total: std::sync::atomic::AtomicU64,
}

impl ReaperHealth {
    /// Fold one sweep's outcome into the counters.
    pub fn record_sweep(&self, report: &SweepReport) {
        use std::sync::atomic::Ordering::Relaxed;
        self.sweeps_total.fetch_add(1, Relaxed);
        self.reaped_total
            .fetch_add(report.reaped.len() as u64, Relaxed);
        self.kill_denied_total
            .fetch_add(u64::from(report.kill_denied), Relaxed);
        self.visibility_ok.store(report.pid1_visible, Relaxed);
    }
}

/// Run one reap sweep with injected boundaries. For each uid-owned process,
/// `decide_reap` applies the exemption rules; orphans are killed via
/// `kill` (SIGTERM → 1s grace → SIGKILL in production). Evaluates the
/// visibility self-test and logs the per-sweep summary at
/// debug level. Returns the full report.
pub fn reap_sweep_with(
    table: &dyn crate::boundaries::proc_table::ProcessTable,
    kill: &dyn Fn(u32) -> std::io::Result<()>,
    cleanup: &dyn Fn(&Path) -> bool,
    ctx: &policy::ReapContext,
) -> SweepReport {
    let mut report = SweepReport {
        pid1_visible: table.pid1_visible(),
        ..SweepReport::default()
    };

    for candidate in table.list_own() {
        report.rows_enumerated += 1;
        match policy::decide_reap(&candidate, ctx) {
            policy::ReapDecision::Reap => {
                report.candidates += 1;
                let exe = candidate.argv.first().map(String::as_str).unwrap_or("?");
                let socket = policy::socket_arg(&candidate.argv).unwrap_or_default();
                match kill(candidate.pid) {
                    Ok(()) => {
                        tracing::info!(
                            pid = candidate.pid,
                            exe,
                            socket = %socket.display(),
                            "reaped orphan daemon"
                        );
                        report.reaped.push(candidate.pid);
                        // Corpse cleanup: remove the
                        // dead orphan's socket + pid files so existence-probing
                        // clients cannot re-latch onto the path.
                        if !socket.as_os_str().is_empty() && cleanup(&socket) {
                            report.corpses_unlinked += 1;
                        }
                    }
                    Err(e) => {
                        if e.kind() == std::io::ErrorKind::PermissionDenied {
                            report.kill_denied += 1;
                        } else {
                            report.kill_failed += 1;
                        }
                        tracing::warn!(pid = candidate.pid, exe, "failed to reap orphan: {e}");
                    }
                }
            }
            policy::ReapDecision::Exempt("not a comb daemon") => {}
            policy::ReapDecision::Exempt(rule) => {
                report.candidates += 1;
                report.tally(rule);
            }
        }
    }

    tracing::debug!(
        rows = report.rows_enumerated,
        candidates = report.candidates,
        exemptions = ?report.exemptions,
        reaped = ?report.reaped,
        kill_denied = report.kill_denied,
        kill_failed = report.kill_failed,
        pid1_visible = report.pid1_visible,
        corpses_unlinked = report.corpses_unlinked,
        "reap sweep summary"
    );

    report
}

/// Remove a reaped orphan's corpse files: the socket itself and the sibling
/// `pid` file. Probes first and NEVER unlinks a serving
/// socket — a racing respawn that re-bound the path between kill and cleanup
/// keeps its socket. Returns true if
/// anything was removed.
pub fn cleanup_orphan_corpse(
    probe: &dyn crate::boundaries::socket::SocketProbe,
    socket: &Path,
) -> bool {
    if probe.is_serving(socket) {
        tracing::info!(socket = %socket.display(), "corpse cleanup skipped: socket is serving again");
        return false;
    }
    let mut removed = false;
    for path in [socket.to_path_buf(), socket.with_file_name("pid")] {
        match std::fs::remove_file(&path) {
            Ok(()) => removed = true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(path = %path.display(), "corpse cleanup failed: {e}");
            }
        }
    }
    if removed {
        tracing::info!(socket = %socket.display(), "removed reaped orphan's corpse files");
    }
    removed
}

/// Production reap sweep: real process table, `supersede_existing` kill
/// semantics (SIGTERM, 1s grace, SIGKILL; already-dead is Ok), corpse cleanup
/// with a real serving probe. Called by the reaping daemon on entering
/// Running and hourly thereafter.
pub fn reap_sweep(our_socket: &Path) -> SweepReport {
    let ctx = policy::ReapContext {
        our_pid: std::process::id(),
        our_socket: our_socket.to_path_buf(),
        grace_age_secs: REAP_GRACE_AGE_SECS,
    };
    reap_sweep_with(
        &crate::boundaries::proc_table::RealProcessTable,
        &|pid| supersede_existing(pid, Duration::from_secs(1)),
        &|socket| cleanup_orphan_corpse(&crate::boundaries::socket::RealSocketProbe, socket),
        &ctx,
    )
}

/// Spawn a thread that watches `current_exe()`. When the binary is modified
/// (after a 200ms debounce window), calls `on_change` once. The watcher thread
/// exits after firing (one-shot).
///
/// Returns Err if the watcher cannot be set up (e.g., current_exe failed). The
/// thread itself logs and exits silently on internal failures (like the
/// notify channel disconnecting).
pub fn spawn_binary_self_watch<F: FnOnce() + Send + 'static>(on_change: F) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    // Canonicalize so we match what the fs-watcher reports (resolves /tmp → /private/tmp on macOS).
    let exe = exe.canonicalize().unwrap_or(exe);
    spawn_binary_self_watch_with(exe, SELF_WATCH_POLL_INTERVAL, true, on_change);
    Ok(())
}

/// Self-watch poll interval. The poll is the guarantee; the
/// fs-event watch is only the fast path.
pub const SELF_WATCH_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Parameterised self-watch: `exe` to supervise, `poll_interval`, and whether
/// to attempt the fs-event fast path (`fs_events: false` is the degraded mode
/// the mtime poll exists for — a stream created without error that delivers
/// nothing, as on sandboxed CI hosts or under a degraded fseventsd). Tests
/// use this seam directly; production goes through
/// [`spawn_binary_self_watch`].
pub fn spawn_binary_self_watch_with<F: FnOnce() + Send + 'static>(
    exe: PathBuf,
    poll_interval: Duration,
    fs_events: bool,
    on_change: F,
) {
    use notify::{EventKind, RecursiveMode, Watcher};
    use std::sync::mpsc;

    std::thread::spawn(move || {
        let start_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        // Watcher setup failure is a degradation, not a defeat: the poll
        // below runs regardless.
        let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
        let watcher = if fs_events {
            match (notify::recommended_watcher(tx), exe.parent()) {
                (Ok(mut w), Some(parent)) => match w.watch(parent, RecursiveMode::NonRecursive) {
                    Ok(()) => {
                        tracing::debug!("self-watch: watching {parent:?} for changes to {exe:?}");
                        Some(w)
                    }
                    Err(e) => {
                        tracing::warn!("self-watch: failed to watch {parent:?}: {e}; poll only");
                        None
                    }
                },
                (Err(e), _) => {
                    tracing::warn!("self-watch: failed to create fs watcher: {e}; poll only");
                    None
                }
                (_, None) => {
                    tracing::warn!("self-watch: current_exe has no parent: {exe:?}; poll only");
                    None
                }
            }
        } else {
            None
        };

        if watcher.is_none() {
            poll_until_changed(&exe, poll_interval, start_unix_ms);
            tracing::info!("daemon binary changed (mtime poll); initiating graceful shutdown");
            on_change();
            return;
        }
        let _watcher = watcher;

        let debounce = Duration::from_millis(200);
        let mut last_event: Option<Instant> = None;
        let mut next_poll = Instant::now() + poll_interval;

        loop {
            let now = Instant::now();
            let debounce_deadline = last_event.map(|t| t + debounce);
            let wake = match debounce_deadline {
                Some(d) => d.min(next_poll),
                None => next_poll,
            };

            match rx.recv_timeout(wake.saturating_duration_since(now)) {
                Ok(Ok(event)) => {
                    tracing::debug!(
                        "self-watch event: kind={:?} paths={:?}",
                        event.kind,
                        event.paths
                    );
                    let path_match = event.paths.iter().any(|p| {
                        let canonical = p.canonicalize().unwrap_or_else(|_| p.clone());
                        canonical == exe || p == &exe
                    });
                    if path_match {
                        let is_change = matches!(
                            event.kind,
                            EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                        );
                        if is_change {
                            last_event = Some(Instant::now());
                        }
                    }
                }
                Ok(Err(e)) => {
                    tracing::error!("fs-watch error: {e}");
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Some(d) = debounce_deadline
                        && Instant::now() >= d
                    {
                        tracing::info!("daemon binary changed; initiating graceful shutdown");
                        on_change();
                        return; // one-shot
                    }
                    if Instant::now() >= next_poll {
                        next_poll = Instant::now() + poll_interval;
                        if binary_changed(&exe, start_unix_ms) {
                            tracing::info!(
                                "daemon binary changed (mtime poll); initiating graceful shutdown"
                            );
                            on_change();
                            return; // one-shot
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // Event channel gone mid-run; the poll carries on alone.
                    poll_until_changed(&exe, poll_interval, start_unix_ms);
                    tracing::info!(
                        "daemon binary changed (mtime poll); initiating graceful shutdown"
                    );
                    on_change();
                    return;
                }
            }
        }
    });
}

/// Block until `exe` has changed (see [`binary_changed`]), checking every
/// `poll_interval`.
fn poll_until_changed(exe: &Path, poll_interval: Duration, start_unix_ms: u64) {
    loop {
        std::thread::sleep(poll_interval);
        if binary_changed(exe, start_unix_ms) {
            return;
        }
    }
}

/// The self-watch's test: `exe`'s mtime is newer than the process start, or
/// `exe` is gone. Homebrew, Nix and mise install each version under its own
/// directory and delete the old one, so the old path vanishes rather than
/// changing. Other stat failures (permissions, say) are not a change.
fn binary_changed(exe: &Path, start_unix_ms: u64) -> bool {
    match binary_newer_than(exe, start_unix_ms) {
        Ok(newer) => newer,
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Returns true iff `binary`'s mtime is strictly newer than `process_start_unix_ms`.
/// Used at daemon startup to catch the rare race where the binary is replaced
/// between process exec and fs-watch registration.
pub fn binary_newer_than(binary: &Path, process_start_unix_ms: u64) -> std::io::Result<bool> {
    let meta = std::fs::metadata(binary)?;
    let mtime = meta.modified()?;
    let mtime_ms = mtime
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Ok(policy::is_binary_newer(mtime_ms, process_start_unix_ms))
}

/// Send SIGTERM to `pid`, wait up to `grace` for graceful exit, then SIGKILL if still alive.
/// Returns Ok once the target is gone; Err on unexpected failure (e.g., permission denied
/// or refusing to kill PID 0/1/self).
/// It is NOT an error for the target to already be dead when called.
pub fn supersede_existing(pid: u32, grace: Duration) -> std::io::Result<()> {
    let pid_t = pid as libc::pid_t;

    // Above i32::MAX the cast is negative: kill() would signal a whole
    // process group, or every process the user owns (-1).
    if pid <= 1 || pid > i32::MAX as u32 || pid == std::process::id() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("refusing to kill pid {pid}"),
        ));
    }

    // SIGTERM
    let rc = unsafe { libc::kill(pid_t, libc::SIGTERM) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Ok(()); // already gone
        }
        return Err(err);
    }

    // Wait for grace period, polling kill(pid, 0).
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        let alive = unsafe { libc::kill(pid_t, 0) };
        if alive != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // SIGKILL if still alive.
    let alive = unsafe { libc::kill(pid_t, 0) };
    if alive == 0 {
        let _ = unsafe { libc::kill(pid_t, libc::SIGKILL) };
        let kill_deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < kill_deadline {
            let still = unsafe { libc::kill(pid_t, 0) };
            if still != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::ESRCH) {
                    return Ok(());
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        return Err(std::io::Error::other(format!("pid {pid} survived SIGKILL")));
    }

    Ok(())
}
