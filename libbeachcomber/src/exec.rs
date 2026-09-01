//! Spawn engine backing the future `exec()` jinja function (registered in a
//! later task — nothing calls [`run`] yet).
//!
//! [`run`] spawns one argv command, writes+closes stdin, and drains
//! stdout/stderr concurrently on their own reader threads while the calling
//! thread polls [`std::process::Child::try_wait`] against a deadline. The
//! concurrent drain is load-bearing, not an optimization: a child writing
//! more than the OS pipe buffer (~64 KiB) to stdout or stderr would block
//! forever if nothing read the other pipe, and `Child::wait_with_output`
//! blocks unboundedly — neither is compatible with a bounded-deadline spawn.
//!
//! Error strings returned here are plain text. They must never contain the
//! substring `"compile error"` — `libbeachcomber-ffi::eval_error_kind`
//! matches that substring to classify eval failures, and the
//! `"expression eval error: "` / `"template render error: "` prefixes that
//! wrap these messages on the eval path are added by the caller
//! (`eval/mod.rs`), not here.

use std::io::{Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// stdout beyond this many bytes is an error, not a truncation.
pub(crate) const MAX_EXEC_STDOUT: usize = 1 << 20;

/// Deadline-loop poll granularity.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// How much of stderr a failure message keeps, in bytes (may split a
/// multi-byte UTF-8 sequence at the boundary; decoded lossily).
const STDERR_ERROR_BYTES: usize = 512;

/// A single argv command to spawn and drain.
pub(crate) struct ExecRequest<'a> {
    pub cmd: &'a str,
    pub args: &'a [String],
    pub stdin: Option<&'a str>,
    pub timeout_ms: u64,
}

/// Spawn `req.cmd` with `req.args`, write+close stdin, and drain stdout and
/// stderr concurrently while polling for exit or `req.timeout_ms`.
///
/// On success, stdout is decoded lossy-UTF-8 with at most one trailing `\n`
/// (and a preceding `\r`, if present) stripped. On failure, the error names
/// `req.cmd`, what happened (a non-zero exit status, a timeout, an oversized
/// stdout, or the spawn `io::Error`), and stderr truncated to
/// [`STDERR_ERROR_BYTES`].
pub(crate) fn run(req: &ExecRequest) -> Result<String, String> {
    let mut child = Command::new(req.cmd)
        .args(req.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("`{}`: failed to spawn: {e}", req.cmd))?;

    write_stdin(&mut child, req.stdin);

    let stdout_capped = Arc::new(AtomicBool::new(false));
    let stdout_thread = spawn_stdout_reader(&mut child, stdout_capped.clone());
    let stderr_thread = spawn_stderr_reader(&mut child);

    let deadline = Instant::now() + Duration::from_millis(req.timeout_ms);
    let outcome = loop {
        if stdout_capped.load(Ordering::Relaxed) {
            break Outcome::Capped;
        }
        if let Ok(Some(status)) = child.try_wait() {
            break Outcome::Exited(status);
        }
        if Instant::now() >= deadline {
            break Outcome::TimedOut;
        }
        thread::sleep(POLL_INTERVAL);
    };

    // Exited children are already reaped by the successful try_wait() above;
    // anything else is still running (or was) and must be killed and reaped
    // before its readers can be joined.
    if !matches!(outcome, Outcome::Exited(_)) {
        let _ = child.kill();
        let _ = child.wait();
    }

    let stdout_buf = stdout_thread.join().unwrap_or_default();
    let stderr_buf = stderr_thread.join().unwrap_or_default();

    match outcome {
        Outcome::Exited(status) if status.success() => Ok(clean_stdout(&stdout_buf)),
        Outcome::Exited(status) => Err(failure_message(
            req.cmd,
            format!("exited with {status}"),
            &stderr_buf,
        )),
        Outcome::TimedOut => Err(failure_message(
            req.cmd,
            format!("timed out after {}ms", req.timeout_ms),
            &stderr_buf,
        )),
        Outcome::Capped => Err(failure_message(
            req.cmd,
            format!("stdout exceeded {MAX_EXEC_STDOUT} bytes"),
            &stderr_buf,
        )),
    }
}

/// How the deadline loop broke.
enum Outcome {
    Exited(ExitStatus),
    TimedOut,
    Capped,
}

/// Write `stdin` (if any) to the child's stdin pipe, then close it. Always
/// closes the pipe, even when `stdin` is `None`, so a child reading from
/// stdin (e.g. `cat`) sees EOF rather than hanging.
fn write_stdin(child: &mut Child, stdin: Option<&str>) {
    let mut handle = child.stdin.take();
    if let (Some(data), Some(pipe)) = (stdin, handle.as_mut()) {
        // Best-effort: a child that exits without reading all of stdin
        // yields a broken-pipe error here, which isn't itself a failure —
        // the exit status and stdout still speak for the run.
        let _ = pipe.write_all(data.as_bytes());
    }
    // `handle` drops here either way, closing the write end.
}

/// Drain the child's stdout on a dedicated thread, stopping (and flagging
/// `capped`) as soon as more than [`MAX_EXEC_STDOUT`] bytes have been read —
/// the caller's deadline loop kills the child on that flag.
fn spawn_stdout_reader(child: &mut Child, capped: Arc<AtomicBool>) -> thread::JoinHandle<Vec<u8>> {
    let mut pipe = child.stdout.take().expect("stdout is piped");
    thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.len() > MAX_EXEC_STDOUT {
                        capped.store(true, Ordering::Relaxed);
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        buf
    })
}

/// Drain the child's stderr on a dedicated thread, unconditionally (only
/// stdout is size-capped).
fn spawn_stderr_reader(child: &mut Child) -> thread::JoinHandle<Vec<u8>> {
    let mut pipe = child.stderr.take().expect("stderr is piped");
    thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(_) => break,
            }
        }
        buf
    })
}

/// Lossy-UTF-8 decode, then strip at most one trailing `\n` (and a
/// preceding `\r`, if present).
fn clean_stdout(buf: &[u8]) -> String {
    let mut s = String::from_utf8_lossy(buf).into_owned();
    if s.ends_with('\n') {
        s.pop();
        if s.ends_with('\r') {
            s.pop();
        }
    }
    s
}

fn failure_message(cmd: &str, reason: String, stderr: &[u8]) -> String {
    let stderr_trunc = truncate_lossy(stderr, STDERR_ERROR_BYTES);
    if stderr_trunc.is_empty() {
        format!("`{cmd}`: {reason}")
    } else {
        format!("`{cmd}`: {reason}: {stderr_trunc}")
    }
}

fn truncate_lossy(bytes: &[u8], limit: usize) -> String {
    let slice = if bytes.len() > limit {
        &bytes[..limit]
    } else {
        bytes
    };
    String::from_utf8_lossy(slice).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_roundtrip_strips_trailing_newline() {
        let req = ExecRequest {
            cmd: "/bin/echo",
            args: &["hello".to_string()],
            stdin: None,
            timeout_ms: 1000,
        };
        assert_eq!(run(&req).expect("echo should succeed"), "hello");
    }

    #[test]
    fn stdin_roundtrip_via_cat() {
        let req = ExecRequest {
            cmd: "cat",
            args: &[],
            stdin: Some("round trip"),
            timeout_ms: 1000,
        };
        assert_eq!(run(&req).expect("cat should succeed"), "round trip");
    }

    #[test]
    fn cat_with_no_stdin_closes_immediately() {
        // `cat` blocks reading stdin until EOF; a fast empty result (well
        // under the 1s timeout) proves stdin is closed immediately when
        // `stdin` is `None`, not left open until the child times out.
        let req = ExecRequest {
            cmd: "cat",
            args: &[],
            stdin: None,
            timeout_ms: 1000,
        };
        let start = Instant::now();
        let out = run(&req).expect("cat should exit as soon as stdin closes");
        assert_eq!(out, "");
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn nonzero_exit_error_carries_stderr() {
        let req = ExecRequest {
            cmd: "sh",
            args: &["-c".to_string(), "echo boom >&2; exit 3".to_string()],
            stdin: None,
            timeout_ms: 1000,
        };
        let err = run(&req).unwrap_err();
        assert!(err.contains("sh"), "should name the command: {err}");
        assert!(
            err.contains("exit status: 3"),
            "should carry the exit status: {err}"
        );
        assert!(err.contains("boom"), "should carry stderr: {err}");
    }

    #[test]
    fn spawn_failure_for_nonexistent_command() {
        let req = ExecRequest {
            cmd: "definitely-not-a-real-command-xyz",
            args: &[],
            stdin: None,
            timeout_ms: 1000,
        };
        let err = run(&req).unwrap_err();
        assert!(err.contains("definitely-not-a-real-command-xyz"), "{err}");
    }

    #[test]
    fn timeout_kills_child() {
        let req = ExecRequest {
            cmd: "sh",
            args: &["-c".to_string(), "sleep 5".to_string()],
            stdin: None,
            timeout_ms: 100,
        };
        let start = Instant::now();
        let err = run(&req).unwrap_err();
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "took {:?}",
            start.elapsed()
        );
        assert!(err.contains("timed out after 100ms"), "{err}");
    }

    #[test]
    fn oversized_stdout_errors() {
        // 2 MB exceeds both MAX_EXEC_STDOUT (1 MiB) and the OS pipe buffer
        // (~64 KiB): a non-concurrent drain would deadlock here until the
        // timeout fired, so a fast, "exceeded" (not "timed out") result also
        // proves stdout and stderr are drained concurrently.
        let req = ExecRequest {
            cmd: "sh",
            args: &["-c".to_string(), "head -c 2000000 /dev/zero".to_string()],
            stdin: None,
            timeout_ms: 5000,
        };
        let start = Instant::now();
        let err = run(&req).unwrap_err();
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "took {:?}",
            start.elapsed()
        );
        assert!(err.contains("exceeded"), "{err}");
    }
}
