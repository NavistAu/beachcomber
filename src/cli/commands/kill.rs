//! Handler for the `kill` subcommand.

use crate::cli::exit::Outcome;
use crate::daemon::is_daemon_running;
use std::path::Path;

pub fn run_kill(socket_path: &Path, timeout: std::time::Duration) -> Outcome {
    if !is_daemon_running(socket_path) {
        println!("Daemon is not running.");
        return Outcome::Done;
    }

    let pid = match resolve_daemon_pid(socket_path) {
        Ok(pid) => pid,
        Err(msg) => {
            crate::cli::report::error("daemon", msg);
            return Outcome::Error;
        }
    };

    // SIGTERM for a clean shutdown; the daemon's signal handler catches it.
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            println!("Daemon process was already stopped.");
            return Outcome::Done;
        }
        crate::cli::report::error("daemon", format_args!("could not signal pid {pid}: {err}"));
        return Outcome::Error;
    }

    // Poll until the socket stops responding.
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if !is_daemon_running(socket_path) {
            println!("Daemon stopped (pid {pid}).");
            return Outcome::Done;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    crate::cli::report::error(
        "daemon",
        format_args!(
            "did not exit within {}; send SIGKILL if needed: kill -9 {pid}",
            libbeachcomber::render::duration(timeout)
        ),
    );
    Outcome::Error
}

/// The pid of the daemon serving `socket_path`, as it reports it itself
/// (`introspect{daemon}`): the one source that cannot go stale.
pub fn resolve_daemon_pid(socket_path: &Path) -> Result<i32, String> {
    let client = crate::client::build_client(socket_path);
    match client.introspect(libbeachcomber::IntrospectSubject::Daemon, None) {
        Ok(libbeachcomber::IntrospectResponse::Daemon(health)) => Ok(health.pid as i32),
        Ok(_) => Err("answered introspect{daemon} with no daemon health".into()),
        Err(e) => Err(format!("is reachable but would not report its pid: {e}")),
    }
}
