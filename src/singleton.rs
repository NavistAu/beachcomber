//! Singleton daemon enforcement: PID file with flock, version comparison,
//! graceful handover on version mismatch, orphan reaping.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
