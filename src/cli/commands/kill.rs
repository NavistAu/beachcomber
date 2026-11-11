//! Handler for the `kill` subcommand.

use crate::cli::exit::Outcome;
use libbeachcomber::spawn::is_serving;
use std::path::Path;
use std::time::Duration;

pub fn run_kill(socket_path: &Path, timeout: Duration) -> Outcome {
    if !is_serving(socket_path) {
        println!("Daemon is not running.");
        return Outcome::Done;
    }
    match stop(socket_path, timeout) {
        Ok(Stopped::Pid(pid)) => {
            println!("Daemon stopped (pid {pid}).");
            Outcome::Done
        }
        Ok(Stopped::AlreadyGone) => {
            println!("Daemon process was already stopped.");
            Outcome::Done
        }
        Err(msg) => {
            crate::cli::report::error("daemon", msg);
            Outcome::Error
        }
    }
}

/// How [`stop`] ended a serving daemon.
pub enum Stopped {
    /// It exited on SIGTERM.
    Pid(i32),
    /// It was gone before the signal reached it.
    AlreadyGone,
}

/// Stop the daemon serving `socket_path` through its own graceful shutdown
/// (SIGTERM), waiting up to `timeout` for the socket to stop answering.
pub fn stop(socket_path: &Path, timeout: Duration) -> Result<Stopped, String> {
    let pid = resolve_daemon_pid(socket_path)?;

    // SIGTERM for a clean shutdown; the daemon's signal handler catches it.
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Ok(Stopped::AlreadyGone);
        }
        return Err(format!("could not signal pid {pid}: {err}"));
    }

    // Poll until the socket stops responding.
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if !is_serving(socket_path) {
            return Ok(Stopped::Pid(pid));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Err(format!(
        "did not exit within {}; send SIGKILL if needed: kill -9 {pid}",
        libbeachcomber::render::duration(timeout)
    ))
}

/// The pid of the daemon serving `socket_path`, as it reports it itself
/// (`introspect{daemon}`): the one source that cannot go stale.
pub fn resolve_daemon_pid(socket_path: &Path) -> Result<i32, String> {
    let client = crate::client::build_client(socket_path);
    match client.introspect(libbeachcomber::IntrospectSubject::Daemon, None) {
        // Only a single process: 0 or a negative pid would signal a group,
        // -1 every process the user owns.
        Ok(libbeachcomber::IntrospectResponse::Daemon(health)) => match i32::try_from(health.pid) {
            Ok(pid) if pid > 1 => Ok(pid),
            _ => Err(format!(
                "reported pid {}, which is not one process",
                health.pid
            )),
        },
        Ok(_) => Err("answered introspect{daemon} with no daemon health".into()),
        Err(e) => Err(format!("is reachable but would not report its pid: {e}")),
    }
}
