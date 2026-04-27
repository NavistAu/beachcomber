//! Handler for the `kill` subcommand.

use crate::cli::introspect_types::DaemonIntrospect;
use crate::daemon::{is_daemon_running, pid_path_for_socket};
use crate::pid_check::pid_is_our_daemon;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

pub fn run_kill(socket_path: &Path, timeout_secs: u64) -> ExitCode {
    if !is_daemon_running(socket_path) {
        println!("Daemon is not running.");
        return ExitCode::SUCCESS;
    }

    let pid_path = pid_path_for_socket(socket_path);
    let pid = match resolve_daemon_pid(&pid_path, socket_path) {
        Ok(pid) => pid,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };

    // SIGTERM for a clean shutdown; the daemon's signal handler catches it.
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            let _ = fs::remove_file(&pid_path);
            println!("Daemon process was already stopped.");
            return ExitCode::SUCCESS;
        }
        eprintln!("Failed to signal daemon (pid {pid}): {err}");
        return ExitCode::from(2);
    }

    // Poll until the socket stops responding.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    while std::time::Instant::now() < deadline {
        if !is_daemon_running(socket_path) {
            println!("Daemon stopped (pid {pid}).");
            return ExitCode::SUCCESS;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    eprintln!(
        "Daemon did not exit within {timeout_secs}s. Send SIGKILL manually if needed: kill -9 {pid}"
    );
    ExitCode::from(1)
}

/// Find the pid of the running daemon. Asks the daemon itself via
/// `introspect{daemon}` — that is the only source that cannot go stale.
/// Falls back to the pid file only if the introspect query doesn't return
/// a pid (older daemons pre-dating the `pid` field on introspect).
pub fn resolve_daemon_pid(pid_path: &Path, socket_path: &Path) -> Result<i32, String> {
    // Authoritative: ask the daemon.
    if let Some(pid) = query_daemon_pid(socket_path) {
        return Ok(pid);
    }

    // Fallback: pid file, but only if the process actually looks like our daemon.
    if let Ok(contents) = fs::read_to_string(pid_path)
        && let Ok(pid) = contents.trim().parse::<i32>()
        && pid > 0
        && pid_is_our_daemon(pid)
    {
        return Ok(pid);
    }

    Err(format!(
        "Daemon is reachable but its pid could not be determined.\n\
         The daemon may predate the `kill` command; upgrade it or restart it with a\n\
         newer binary, then try again. (Checked pid file: {})",
        pid_path.display()
    ))
}

/// Open a one-shot connection to the daemon and read `pid` out of the introspect{daemon} response.
fn query_daemon_pid(socket_path: &Path) -> Option<i32> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let mut stream = UnixStream::connect(socket_path).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .ok()?;
    stream
        .write_all(b"{\"op\":\"introspect\",\"subject\":\"daemon\"}\n")
        .ok()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;

    // Parse the full envelope first, then deserialise the `data` field into
    // DaemonIntrospect.  Using a typed struct here prevents the class of bug
    // where a field name or shape changes silently (e.g. the historical
    // regression where `status` returned cache rows instead of daemon fields,
    // causing `comb kill` to read a pid of `None`).
    let envelope: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let data = envelope.get("data")?;
    let introspect: DaemonIntrospect = serde_json::from_value(data.clone()).ok()?;
    Some(introspect.pid as i32)
}
