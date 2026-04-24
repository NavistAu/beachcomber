//! Singleton daemon enforcement: PID file with flock, version comparison,
//! graceful handover on version mismatch, orphan reaping.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct PidFileRecord {
    pub pid: u32,
    pub version: String,
    pub binary: PathBuf,
    pub started_unix_ms: u64,
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
    pub fn acquire(pid_path: &Path, version: &str) -> Result<Self, SingletonLockError> {
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

/// Given the existing singleton's record and our own version, decide whether
/// to supersede (kill and take over) or exit silently (existing daemon is fine).
pub fn decide_supersession(existing: &PidFileRecord, our_version: &str) -> SupersessionDecision {
    if existing.version == our_version {
        SupersessionDecision::ExitSilent
    } else {
        SupersessionDecision::Supersede {
            existing_pid: existing.pid,
        }
    }
}

/// High-level: acquire the singleton lock, superseding an existing daemon if its
/// version differs from ours.
///
/// Returns:
/// - `Ok(Some(lock))`: we hold the singleton; run the daemon.
/// - `Ok(None)`: existing daemon has the same version; caller should exit silently.
/// - `Err(...)`: unexpected failure (IO, permission, etc.).
pub fn acquire_or_supersede(
    pid_path: &Path,
    our_version: &str,
) -> Result<Option<SingletonLock>, SingletonLockError> {
    match SingletonLock::acquire(pid_path, our_version) {
        Ok(lock) => Ok(Some(lock)),
        Err(SingletonLockError::AlreadyHeld { existing: Some(rec) }) => {
            match decide_supersession(&rec, our_version) {
                SupersessionDecision::ExitSilent => Ok(None),
                SupersessionDecision::Supersede { existing_pid } => {
                    supersede_existing(existing_pid, Duration::from_secs(1))?;
                    // Retry acquire briefly; old daemon may take a moment to release.
                    let deadline = Instant::now() + Duration::from_secs(2);
                    loop {
                        match SingletonLock::acquire(pid_path, our_version) {
                            Ok(lock) => return Ok(Some(lock)),
                            Err(SingletonLockError::AlreadyHeld { .. }) if Instant::now() < deadline => {
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

/// Spawn a thread that watches `current_exe()`. When the binary is modified
/// (after a 200ms debounce window), calls `on_change` once. The watcher thread
/// exits after firing (one-shot).
///
/// Returns Err if the watcher cannot be set up (e.g., current_exe failed). The
/// thread itself logs and exits silently on internal failures (like the
/// notify channel disconnecting).
pub fn spawn_binary_self_watch<F: FnOnce() + Send + 'static>(
    on_change: F,
) -> std::io::Result<()> {
    use notify::{EventKind, RecursiveMode, Watcher};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let exe = std::env::current_exe()?;
    // Canonicalize so we match what the fs-watcher reports (resolves /tmp → /private/tmp on macOS).
    let exe = exe.canonicalize().unwrap_or(exe);

    std::thread::spawn(move || {
        let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
        let mut watcher = match notify::recommended_watcher(tx) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!("failed to create fs watcher for self-watch: {e}");
                return;
            }
        };
        let parent = match exe.parent() {
            Some(p) => p.to_path_buf(),
            None => {
                tracing::error!("current_exe has no parent: {exe:?}");
                return;
            }
        };
        tracing::debug!("self-watch: watching {parent:?} for changes to {exe:?}");
        if let Err(e) = watcher.watch(&parent, RecursiveMode::NonRecursive) {
            tracing::error!("failed to watch {parent:?}: {e}");
            return;
        }

        let mut last_event: Option<Instant> = None;
        let debounce = Duration::from_millis(200);

        loop {
            let timeout = last_event
                .map(|t| (t + debounce).saturating_duration_since(Instant::now()))
                .unwrap_or(Duration::from_secs(60));

            match rx.recv_timeout(timeout) {
                Ok(Ok(event)) => {
                    tracing::debug!("self-watch event: kind={:?} paths={:?}", event.kind, event.paths);
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
                    if let Some(t) = last_event
                        && Instant::now() >= t + debounce
                    {
                        tracing::info!("daemon binary changed; initiating graceful shutdown");
                        on_change();
                        return; // one-shot
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    });

    Ok(())
}

/// Send SIGTERM to `pid`, wait up to `grace` for graceful exit, then SIGKILL if still alive.
/// Returns Ok once the target is gone; Err on unexpected failure (e.g., permission denied
/// or refusing to kill PID 0/1/self).
/// It is NOT an error for the target to already be dead when called.
pub fn supersede_existing(pid: u32, grace: Duration) -> std::io::Result<()> {
    let pid_t = pid as libc::pid_t;

    if pid <= 1 || pid == std::process::id() {
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
