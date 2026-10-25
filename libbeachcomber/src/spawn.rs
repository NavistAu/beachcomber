//! Starting a daemon: one spawner for the CLI and every SDK.
//!
//! The caller says which `comb` to run (the CLI runs itself; the library the
//! first `comb` on `PATH`), whether the daemon is spared reaping, and where
//! its stderr goes. Everything else is the same for both: the socket's
//! directory is made, the daemon's stderr is appended to its log — so what it
//! prints before its own logger starts, such as a configuration it refuses,
//! is kept — and the wait for its socket follows one backoff.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// How to start a daemon.
#[derive(Clone, Copy, Debug)]
pub struct Spawn<'a> {
    /// The `comb` to run.
    pub binary: &'a Path,
    /// Mark the daemon exempt from reaping: its socket is one this process
    /// chose ([`crate::socket::SocketSource::no_reap`]).
    pub no_reap: bool,
    /// The file its stderr is appended to: the daemon's log ([`log_path`]).
    pub log: &'a Path,
}

/// Whether a daemon accepts connections on `socket`.
pub fn is_serving(socket: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket).is_ok()
}

/// A daemon serving `socket`: the one already there, or one `spawn` starts,
/// waited for. A socket path too long to bind is refused before anything is
/// spawned (`InvalidInput`); a daemon that never comes up is `TimedOut`.
pub fn ensure(socket: &Path, spawn: &Spawn<'_>) -> std::io::Result<()> {
    crate::socket::check_length(socket)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    if is_serving(socket) {
        return Ok(());
    }
    start(socket, spawn)?;
    if !wait(socket) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "daemon failed to start within timeout",
        ));
    }
    Ok(())
}

/// Start `comb daemon --socket <socket>` in the background.
fn start(socket: &Path, spawn: &Spawn<'_>) -> std::io::Result<()> {
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if let Some(dir) = spawn.log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let stderr = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(spawn.log)
    {
        Ok(file) => std::process::Stdio::from(file),
        Err(_) => std::process::Stdio::null(),
    };
    let mut cmd = std::process::Command::new(spawn.binary);
    cmd.arg("daemon").arg("--socket").arg(socket.as_os_str());
    if spawn.no_reap {
        cmd.arg("--no-reap");
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(stderr)
        .spawn()?;
    Ok(())
}

/// Poll `socket` until a daemon accepts connections: 10ms, doubling, capped
/// at 500ms, eight tries. False when it never does.
pub fn wait(socket: &Path) -> bool {
    let mut delay = Duration::from_millis(10);
    for _ in 0..WAIT_ATTEMPTS {
        if is_serving(socket) {
            return true;
        }
        std::thread::sleep(delay);
        delay = next_delay(delay);
    }
    is_serving(socket)
}

/// How many times [`wait`] sleeps before giving up.
const WAIT_ATTEMPTS: u32 = 8;

/// The backoff between two tries of [`wait`].
fn next_delay(delay: Duration) -> Duration {
    (delay * 2).min(Duration::from_millis(500))
}

/// The daemon's log: `$XDG_STATE_HOME/beachcomber/daemon.log`, with
/// `$XDG_STATE_HOME` an absolute path, else `~/.local/state/beachcomber/daemon.log`.
pub fn log_path() -> PathBuf {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("state"))
        })
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    state.join("beachcomber").join("daemon.log")
}

/// The first executable `comb` on `PATH`, symlinks resolved.
pub fn find_comb() -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("comb"))
        .find(|candidate| {
            std::fs::metadata(candidate)
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .map(|found| std::fs::canonicalize(&found).unwrap_or(found))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backoff_doubles_and_caps_at_half_a_second() {
        assert_eq!(
            next_delay(Duration::from_millis(10)),
            Duration::from_millis(20)
        );
        assert_eq!(
            next_delay(Duration::from_millis(400)),
            Duration::from_millis(500)
        );
        assert_eq!(
            next_delay(Duration::from_millis(500)),
            Duration::from_millis(500)
        );
    }

    #[test]
    fn the_log_is_under_xdg_state_home_when_it_is_absolute() {
        // SAFETY: nextest runs every test in its own process.
        unsafe { std::env::set_var("XDG_STATE_HOME", "/state") };
        assert_eq!(log_path(), PathBuf::from("/state/beachcomber/daemon.log"));
        unsafe { std::env::set_var("XDG_STATE_HOME", "relative") };
        unsafe { std::env::set_var("HOME", "/h") };
        assert_eq!(
            log_path(),
            PathBuf::from("/h/.local/state/beachcomber/daemon.log")
        );
    }

    #[test]
    fn find_comb_skips_a_comb_that_is_not_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("bc-find-comb-{}", std::process::id()));
        let (a, b) = (dir.join("a"), dir.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("comb"), "").unwrap();
        std::fs::write(b.join("comb"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(b.join("comb"), std::fs::Permissions::from_mode(0o755)).unwrap();
        // SAFETY: nextest runs every test in its own process.
        unsafe { std::env::set_var("PATH", std::env::join_paths([&a, &b]).unwrap()) };
        let found = find_comb().expect("the executable one");
        assert_eq!(found, std::fs::canonicalize(b.join("comb")).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
