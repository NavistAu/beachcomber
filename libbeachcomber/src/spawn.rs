//! Starting a daemon: one spawner for the CLI and every SDK.
//!
//! The caller says which `comb` to run (the CLI runs itself; the library the
//! first `comb` on `PATH`), whether the daemon is spared reaping, and where
//! its stderr goes. Everything else is the same for both: the socket's
//! directory is made, the daemon's stderr is appended to its log — so what it
//! prints before its own logger starts, such as a configuration it refuses,
//! is kept — and the daemon says when it serves.
//!
//! It says so on a pipe: the spawner hands it the write end (`comb daemon
//! --ready-fd <fd>`), and the daemon writes a newline there once its socket
//! is bound, or once it finds a daemon of its own build already serving it.
//! A daemon that exits first closes the pipe unwritten, so the spawner learns
//! of the exit the moment it happens. Nothing polls the socket.

use std::io::{Read, Seek};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

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

/// How long [`ensure`] waits for a daemon that neither says it serves nor
/// exits. A daemon that does either is answered at once; this bounds only a
/// daemon wedged before its socket is bound, which is left to run.
pub const READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Why a daemon could not be started.
#[derive(Debug)]
pub enum Error {
    /// The socket path is too long to bind; nothing was spawned.
    Socket(String),
    /// The daemon could not be run, or its readiness pipe made or read.
    Io(std::io::Error),
    /// The daemon exited before it served. `said` is the first error line
    /// it wrote to its log meanwhile, or its last line when none reads as an
    /// error; `None` when it wrote nothing or its stderr went elsewhere.
    Exited {
        status: ExitStatus,
        said: Option<String>,
    },
    /// The daemon neither served nor exited within this long. It is left
    /// running.
    TimedOut(Duration),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Socket(why) => f.write_str(why),
            Error::Io(e) => write!(f, "{e}"),
            Error::Exited { status, said: None } => write!(f, "exited before serving ({status})"),
            Error::Exited {
                status,
                said: Some(said),
            } => write!(f, "exited before serving ({status}): {said}"),
            Error::TimedOut(wait) => {
                write!(f, "did not serve within {}", crate::render::duration(*wait))
            }
        }
    }
}

impl std::error::Error for Error {}

/// Whether a daemon accepts connections on `socket`.
pub fn is_serving(socket: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket).is_ok()
}

/// A daemon serving `socket`: the one already there, or one `spawn` starts
/// (`comb daemon --socket <socket>`, `--no-reap` when asked), detached, and
/// waited for up to [`READY_TIMEOUT`]. A socket path too long to bind is
/// refused before anything is spawned.
pub fn ensure(socket: &Path, spawn: &Spawn<'_>) -> Result<(), Error> {
    crate::socket::check_length(socket).map_err(Error::Socket)?;
    if is_serving(socket) {
        return Ok(());
    }
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir).map_err(Error::Io)?;
    }
    let mut cmd = Command::new(spawn.binary);
    cmd.arg("daemon").arg("--socket").arg(socket.as_os_str());
    if spawn.no_reap {
        cmd.arg("--no-reap");
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::null());
    launch(&mut cmd, Some(spawn.log), READY_TIMEOUT).map(drop)
}

/// Run `cmd`, a `comb daemon` command line, with a readiness pipe
/// (`--ready-fd` is appended), and wait up to `wait` for the daemon to say it
/// serves; the child is returned once it has. With a `log`, the daemon's
/// stderr is appended to it, and an exit reports what it wrote there.
pub fn launch(cmd: &mut Command, log: Option<&Path>, wait: Duration) -> Result<Child, Error> {
    let log = log.map(|log| (log, open_log(log, cmd)));
    let (mut ready, writer) = std::io::pipe().map_err(Error::Io)?;
    let writer = clear_of_stdio(writer.into()).map_err(Error::Io)?;
    let fd = writer.as_raw_fd();
    cmd.arg("--ready-fd").arg(fd.to_string());
    // SAFETY: `fcntl` is async-signal-safe, and runs in the child between
    // fork and exec: only the daemon inherits the write end.
    unsafe {
        cmd.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(Error::Io)?;
    // The daemon holds the only write end now: its exit is end-of-file here.
    drop(writer);
    match said_ready(&mut ready, wait) {
        Ok(true) => Ok(child),
        Ok(false) => {
            let status = child.wait().map_err(Error::Io)?;
            let said = log.and_then(|(log, from)| written_since(log, from?));
            Err(Error::Exited { status, said })
        }
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Err(Error::TimedOut(wait)),
        Err(e) => Err(Error::Io(e)),
    }
}

/// Send `cmd`'s stderr to the end of `log`, its directory made, and answer
/// where its writing starts; a log that cannot be opened takes nothing.
fn open_log(log: &Path, cmd: &mut Command) -> Option<u64> {
    if let Some(dir) = log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
    {
        Ok(file) => {
            let from = file.metadata().map(|m| m.len()).ok();
            cmd.stderr(file);
            from
        }
        Err(_) => {
            cmd.stderr(Stdio::null());
            None
        }
    }
}

/// `fd`, numbered 3 or above: the child's stdin, stdout and stderr are set
/// up on 0 to 2 before the write end is handed over, so a pipe made in a
/// process with one of those closed would be overwritten.
fn clear_of_stdio(fd: OwnedFd) -> std::io::Result<OwnedFd> {
    if fd.as_raw_fd() > 2 {
        return Ok(fd);
    }
    // `try_clone` duplicates onto the lowest free number from 3.
    fd.try_clone()
}

/// Whether the daemon wrote its readiness pipe (true) or closed it unwritten
/// (false) within `wait`; `TimedOut` when it did neither.
fn said_ready(ready: &mut std::io::PipeReader, wait: Duration) -> std::io::Result<bool> {
    let deadline = Instant::now() + wait;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let mut pfd = libc::pollfd {
            fd: ready.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = i32::try_from(left.as_millis()).unwrap_or(i32::MAX);
        // SAFETY: one valid pollfd, for the duration of the call.
        match unsafe { libc::poll(&mut pfd, 1, ms) } {
            -1 => {
                let e = std::io::Error::last_os_error();
                if e.kind() != std::io::ErrorKind::Interrupted {
                    return Err(e);
                }
            }
            0 => return Err(std::io::ErrorKind::TimedOut.into()),
            _ => {
                let mut byte = [0u8; 1];
                loop {
                    match ready.read(&mut byte) {
                        Ok(n) => return Ok(n == 1),
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(e) => return Err(e),
                    }
                }
            }
        }
    }
}

/// What the daemon wrote to `log` from byte `from` on ([`what_it_said`]).
fn written_since(log: &Path, from: u64) -> Option<String> {
    let mut file = std::fs::File::open(log).ok()?;
    file.seek(std::io::SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::new();
    file.take(64 * 1024).read_to_end(&mut bytes).ok()?;
    what_it_said(&String::from_utf8_lossy(&bytes))
}

/// The first line of `text` that reads as an error — `error: …`, as a
/// refusal and clap print, or a log line at `ERROR` — else its last line.
fn what_it_said(text: &str) -> Option<String> {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    lines
        .clone()
        .find(|line| line.starts_with("error:") || line.contains(" ERROR "))
        .or_else(|| lines.next_back())
        .map(str::to_string)
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
    fn what_a_daemon_said_is_its_first_error_line_else_its_last() {
        let refusal = "error: [providers] is unknown\nerror: daemon: refusing to start: the configuration has 1 error(s)\n";
        assert_eq!(
            what_it_said(refusal).as_deref(),
            Some("error: [providers] is unknown")
        );
        let logged = "2026-10-26T08:00:00Z  INFO beachcomber: Starting\n2026-10-26T08:00:00Z ERROR beachcomber::daemon: Server error: in use\n";
        assert_eq!(
            what_it_said(logged).as_deref(),
            Some("2026-10-26T08:00:00Z ERROR beachcomber::daemon: Server error: in use")
        );
        assert_eq!(what_it_said("first\n  last  \n\n").as_deref(), Some("last"));
        assert_eq!(what_it_said("\n \n"), None);
    }

    #[test]
    fn a_pipe_on_a_stdio_number_is_moved_above_it() {
        use std::os::fd::FromRawFd;
        // SAFETY: a test process whose stdin nothing reads; 0 is restored below.
        let saved = unsafe { libc::dup(0) };
        assert!(saved > 2);
        let (reader, writer) = std::io::pipe().unwrap();
        // SAFETY: `dup2` onto 0 makes 0 a second write end this test owns.
        assert_eq!(unsafe { libc::dup2(writer.as_raw_fd(), 0) }, 0);
        let on_stdin = unsafe { OwnedFd::from_raw_fd(0) };
        let moved = clear_of_stdio(on_stdin).unwrap();
        assert!(moved.as_raw_fd() > 2, "{}", moved.as_raw_fd());
        drop((reader, writer, moved));
        // SAFETY: put stdin back.
        unsafe {
            libc::dup2(saved, 0);
            libc::close(saved);
        }
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
