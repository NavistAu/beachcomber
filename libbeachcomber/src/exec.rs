//! Spawn engine backing the future `exec()` jinja function (registered in a
//! later task — nothing calls [`run`] yet).
//!
//! [`run`] spawns one argv command in its own process group, writes+closes
//! stdin, and drains stdout/stderr concurrently — each on its own thread —
//! while the calling thread polls [`std::process::Child::try_wait`] against
//! a deadline. The whole point is a *bounded* deadline: `run` must never
//! block meaningfully longer than `timeout_ms` (plus a small fixed grace),
//! no matter what the child does. Three things threaten that, and the
//! shape of this module is the fix for all three:
//!
//! - **Stdin can deadlock against stdout/stderr.** A child that both reads a
//!   large stdin payload and writes a large amount of output can block
//!   forever if the write end of stdin isn't drained concurrently with the
//!   read ends of stdout/stderr — three parties, three threads, all started
//!   before anyone blocks. Stdin is written on its own thread, spawned
//!   alongside the two readers, never on the thread computing the deadline.
//! - **The deadline must cover everything, including the stdin write.** It's
//!   computed immediately after spawn, before anything that could block.
//! - **A grandchild can hold a pipe open after the child we asked for is
//!   gone.** `sh -c "cmd &"` backgrounds a process that inherits stdout and
//!   stderr; killing just the immediate child leaves that descendant
//!   holding the write end, so a reader thread's `read()` never sees EOF.
//!   The child is spawned in its own process group
//!   (`CommandExt::process_group(0)`), and this function kills the whole
//!   group (`libc::killpg`, SIGKILL) once the primary child is done —
//!   **on every outcome, including a clean exit**: an `exec()` call on a
//!   statusline render path has no legitimate reason to leave a daemon
//!   child running past its own return, so this module doesn't try to
//!   support that. Even so, a reader thread's final receive is bounded by
//!   [`READER_JOIN_GRACE`] rather than an unconditional join/wait — in case
//!   some descendant manages to escape the group (e.g. by calling
//!   `setsid`), a `run` call still returns instead of hanging on it.
//!
//! Error strings returned here are plain text. They must never contain the
//! substring `"compile error"` — `libbeachcomber-ffi::eval_error_kind`
//! matches that substring to classify eval failures, and the
//! `"expression eval error: "` / `"template render error: "` prefixes that
//! wrap these messages on the eval path are added by the caller
//! (`eval/mod.rs`), not here.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{ChildStdin, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// stdout beyond this many bytes is an error, not a truncation.
pub(crate) const MAX_EXEC_STDOUT: usize = 1 << 20;

/// Deadline-loop poll granularity: starts tight (most commands finish in a
/// handful of milliseconds, and this runs on a statusline render path where
/// a flat 10ms first poll is a real, measurable cost) and doubles up to
/// this ceiling so a long-running or hung command doesn't busy-poll.
const POLL_INTERVAL_MIN: Duration = Duration::from_millis(1);
const POLL_INTERVAL_MAX: Duration = Duration::from_millis(10);

/// How much of stderr a failure message keeps, in bytes (may split a
/// multi-byte UTF-8 sequence at the boundary; decoded lossily).
const STDERR_ERROR_BYTES: usize = 512;

/// How much of stderr the reader thread *retains* while draining — a small
/// multiple of [`STDERR_ERROR_BYTES`], generous enough that the final
/// truncation in [`failure_message`] is never starved of material, but
/// bounded so a chatty child's stderr can't grow this buffer unboundedly.
/// The reader keeps reading past this limit (a child that fills its stderr
/// pipe and blocks on it would never finish), it just stops appending.
const STDERR_RETENTION_BYTES: usize = STDERR_ERROR_BYTES * 4;

/// Bound on the final stdout/stderr handoff after the process group has
/// been killed. Under normal operation (no escaped descendants) the reader
/// threads finish essentially immediately once every write-end holder is
/// dead; this is a defensive ceiling, not an expected wait.
const READER_JOIN_GRACE: Duration = Duration::from_millis(300);

/// A single argv command to spawn and drain.
pub(crate) struct ExecRequest<'a> {
    pub cmd: &'a str,
    pub args: &'a [String],
    pub stdin: Option<&'a str>,
    pub timeout_ms: u64,
}

/// How the deadline loop broke.
enum Outcome {
    Exited(ExitStatus),
    TimedOut,
    Capped,
    /// `Child::try_wait` itself returned an `io::Error` — treated like a
    /// timeout for cleanup purposes (kill and reap defensively) since the
    /// child's true state is unknown.
    WaitFailed(std::io::Error),
}

/// Spawn `req.cmd` with `req.args`, write+close stdin, and drain stdout and
/// stderr concurrently while polling for exit or `req.timeout_ms`. See the
/// module doc for the full bounded-deadline contract.
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
        .process_group(0)
        .spawn()
        .map_err(|e| format!("`{}`: failed to spawn: {e}", req.cmd))?;

    // Computed immediately after spawn, before anything that could block —
    // in particular, before the stdin write (now threaded, see below), which
    // used to run synchronously right here and left the deadline blind to
    // however long it took.
    let deadline = Instant::now() + Duration::from_millis(req.timeout_ms);
    // `process_group(0)` makes the child its own group leader, so its pid
    // is also the process group id — the target for the group-wide kill
    // once we're done (see module doc).
    let pgid = child.id() as libc::pid_t;

    let stdin_pipe = child.stdin.take().expect("stdin is piped");
    let stdout_pipe = child.stdout.take().expect("stdout is piped");
    let stderr_pipe = child.stderr.take().expect("stderr is piped");

    // Fire-and-forget: nothing here needs this thread's result, and it
    // holds no resource this function must wait on. If the child never
    // reads stdin, the write blocks until the process group dies (below),
    // at which point it fails with a broken pipe and the thread exits.
    // `.to_string()` is required here, not just convenient: the thread is
    // `'static` (unscoped, so it can outlive this call — see module doc on
    // the join grace), so it can't borrow `req.stdin`.
    spawn_stdin_writer(stdin_pipe, req.stdin.map(str::to_string));

    // A pure boolean latch, never used to guard access to other memory, so
    // `Relaxed` is enough: the poll loop below tolerates a few milliseconds
    // of staleness (it just reacts to the cap on the next tick), and the
    // buffer data itself never crosses through this flag — only through
    // the `mpsc` channels below, which supply their own synchronization.
    let stdout_capped = Arc::new(AtomicBool::new(false));
    let stdout_rx = spawn_reader(stdout_pipe, MAX_EXEC_STDOUT, Some(stdout_capped.clone()));
    let stderr_rx = spawn_reader(stderr_pipe, STDERR_RETENTION_BYTES, None);

    let mut interval = POLL_INTERVAL_MIN;
    let outcome = loop {
        if stdout_capped.load(Ordering::Relaxed) {
            break Outcome::Capped;
        }
        match child.try_wait() {
            Ok(Some(status)) => break Outcome::Exited(status),
            Ok(None) => {}
            Err(e) => break Outcome::WaitFailed(e),
        }
        let now = Instant::now();
        if now >= deadline {
            break Outcome::TimedOut;
        }
        thread::sleep(interval.min(deadline - now));
        interval = (interval * 2).min(POLL_INTERVAL_MAX);
    };

    // Always kill the whole process group first — including on a clean
    // exit — so a descendant holding a pipe write end can't leave a reader
    // thread blocked past this function's return. See module doc. This
    // must happen *before* any `wait()` below: `Child::wait()` blocks until
    // the child actually exits, so calling it first (with nothing having
    // signalled the child yet) would just wait out the child's natural
    // runtime instead of the deadline. `child.kill()` targets the pid
    // directly, alongside `killpg` targeting the group it leads — belt and
    // suspenders, so nothing here still depends on a single signal landing.
    // Reusing `pgid` here is safe even though we may already have reaped
    // the leader (in the `Exited` case, via `try_wait` above): POSIX keeps
    // a process group id reserved for as long as any member process is
    // still in it, so it cannot have been recycled for something unrelated
    // in the handful of instructions since.
    let _ = child.kill();
    let _ = unsafe { libc::killpg(pgid, libc::SIGKILL) };
    // `Exited` already reaped the child via the successful try_wait() above;
    // anything else leaves it (possibly still running) unreaped.
    if !matches!(outcome, Outcome::Exited(_)) {
        let _ = child.wait();
    }

    // One shared grace budget for both receives (M1), not one full
    // READER_JOIN_GRACE each — a stdout reader that eats the whole grace
    // window leaves stderr's receive nothing to wait on, rather than a
    // second full grace on top.
    let recv_deadline = Instant::now() + READER_JOIN_GRACE;
    let stdout_result =
        stdout_rx.recv_timeout(recv_deadline.saturating_duration_since(Instant::now()));
    let stderr_buf = stderr_rx
        .recv_timeout(recv_deadline.saturating_duration_since(Instant::now()))
        .unwrap_or_default();

    // Narrow race: the poll loop's cap check and the reader thread crossing
    // MAX_EXEC_STDOUT can interleave such that the child is observed to
    // exit cleanly (or the deadline fires) just before, or concurrently
    // with, the cap being flagged. Re-check post-hoc so neither a
    // successful exit nor a slow oversized writer's timeout can mask an
    // oversized stdout.
    let outcome = match outcome {
        Outcome::Exited(status) if status.success() && stdout_capped.load(Ordering::Relaxed) => {
            Outcome::Capped
        }
        Outcome::TimedOut if stdout_capped.load(Ordering::Relaxed) => Outcome::Capped,
        other => other,
    };

    match outcome {
        Outcome::Exited(status) if status.success() => {
            stdout_outcome(stdout_result, req.cmd, &stderr_buf)
        }
        Outcome::Exited(status) => Err(failure_message(req.cmd, format!("{status}"), &stderr_buf)),
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
        Outcome::WaitFailed(e) => Err(failure_message(
            req.cmd,
            format!("wait failed: {e}"),
            &stderr_buf,
        )),
    }
}

/// Turn the stdout reader's final receive into the success/failure result a
/// clean exit reports. `Ok` means the reader actually sent its buffer back;
/// a `Timeout` (abandoned after [`READER_JOIN_GRACE`]) or `Disconnected`
/// (reader thread panicked) must still be a *failure*, not an empty
/// success — a descendant that outlived the process-group kill may be
/// holding real, unread output on the other end of that pipe. Split out of
/// [`run`] so this failure path is unit-testable without spawning a
/// process (see the `stdout_outcome_*` tests).
fn stdout_outcome(
    result: Result<Vec<u8>, mpsc::RecvTimeoutError>,
    cmd: &str,
    stderr: &[u8],
) -> Result<String, String> {
    match result {
        Ok(buf) => Ok(clean_stdout(&buf)),
        Err(e) => Err(failure_message(
            cmd,
            format!(
                "output reader abandoned after grace ({e}); a descendant process may still hold the pipe"
            ),
            stderr,
        )),
    }
}

/// Write `data` (if any) to the child's stdin pipe on a dedicated thread,
/// then close it. Always closes the pipe, even when `data` is `None`, so a
/// child reading from stdin (e.g. `cat`) sees EOF rather than hanging.
fn spawn_stdin_writer(mut pipe: ChildStdin, data: Option<String>) {
    thread::spawn(move || {
        if let Some(data) = data {
            // Best-effort: a child that exits without reading all of stdin
            // yields a broken-pipe error here, which isn't itself a
            // failure — the exit status and stdout still speak for the run.
            let _ = pipe.write_all(data.as_bytes());
        }
        // `pipe` drops here either way, closing the write end.
    });
}

/// Drain `pipe` on its own thread until EOF (so the child is never blocked
/// writing to it), handing the collected bytes back over an `mpsc` channel
/// rather than a plain `JoinHandle` — see [`recv_or_default`].
///
/// At most `retain_limit` bytes are kept in the returned buffer; bytes read
/// past that are still drained (to keep the child unblocked) but discarded.
///
/// When `hard_cap` is `Some`, exceeding `retain_limit` also stops the read
/// early and flags it — used only for stdout, whose overflow is a hard
/// command error (see [`MAX_EXEC_STDOUT`]), not just a truncation. Stderr
/// passes `None`: it keeps draining to completion, only its *retention* is
/// bounded.
fn spawn_reader<R>(
    mut pipe: R,
    retain_limit: usize,
    hard_cap: Option<Arc<AtomicBool>>,
) -> mpsc::Receiver<Vec<u8>>
where
    R: Read + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let mut total: usize = 0;
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    total += n;
                    if buf.len() < retain_limit {
                        let keep = (retain_limit - buf.len()).min(n);
                        buf.extend_from_slice(&chunk[..keep]);
                    }
                    if let Some(capped) = &hard_cap
                        && total > retain_limit
                    {
                        capped.store(true, Ordering::Relaxed);
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        // A caller that already gave up (recv_timeout elapsed) has dropped
        // its receiver; the send below simply fails and is ignored, and the
        // thread exits normally right after.
        let _ = tx.send(buf);
    });
    rx
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
    fn large_stdin_roundtrip_via_cat() {
        // 256 KiB comfortably exceeds the ~64 KiB OS pipe buffer in both
        // directions (stdin in, stdout out): writing stdin on the main
        // thread, or draining stdout only after the write finished, would
        // deadlock here (C1).
        let payload = "x".repeat(256 * 1024);
        let req = ExecRequest {
            cmd: "cat",
            args: &[],
            stdin: Some(&payload),
            timeout_ms: 3000,
        };
        let start = Instant::now();
        let out = run(&req).expect("cat should succeed");
        assert_eq!(out, payload);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn timeout_covers_the_stdin_write() {
        // C2 repro: `sleep` never reads stdin, so a 256 KiB payload (past
        // the pipe buffer) blocks the write until something reads it or the
        // pipe breaks. If the deadline were computed (or the write
        // performed) before threading, this would time out at ~3s instead
        // of ~200ms.
        let payload = "x".repeat(256 * 1024);
        let req = ExecRequest {
            cmd: "sh",
            args: &["-c".to_string(), "sleep 3".to_string()],
            stdin: Some(&payload),
            timeout_ms: 200,
        };
        let start = Instant::now();
        let err = run(&req).unwrap_err();
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "took {:?}",
            start.elapsed()
        );
        assert!(err.contains("timed out after 200ms"), "{err}");
    }

    #[test]
    fn grandchild_does_not_block_success_path() {
        // `sh` exits successfully almost immediately, but `sleep 3 &`
        // backgrounds a grandchild that inherits the stdout/stderr pipes.
        // Without an explicit post-exit process-group kill (C3), the
        // readers would block on that grandchild's open fds until IT exits
        // (~3s) even though the command we asked for already finished.
        let req = ExecRequest {
            cmd: "sh",
            args: &["-c".to_string(), "sleep 3 & echo started".to_string()],
            stdin: None,
            timeout_ms: 200,
        };
        let start = Instant::now();
        let out = run(&req).expect("sh should succeed");
        assert_eq!(out, "started");
        assert!(
            start.elapsed() < Duration::from_secs(1),
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
            "should carry the exit status, without doubled wording: {err}"
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

    #[test]
    fn cap_beats_a_tight_timeout() {
        // Same oversized writer, but with a timeout so tight (100ms) that a
        // buggy ordering (checking the deadline before the cap flag, or
        // detecting the cap too slowly) could plausibly report a timeout
        // instead. The cap must still win, and well within the 100ms
        // budget itself.
        let req = ExecRequest {
            cmd: "sh",
            args: &["-c".to_string(), "head -c 2000000 /dev/zero".to_string()],
            stdin: None,
            timeout_ms: 100,
        };
        let start = Instant::now();
        let err = run(&req).unwrap_err();
        // The real assertion is which error wins (below); elapsed time is
        // just a generous sanity check that this didn't fall back to
        // waiting out some much larger timeout.
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "took {:?}",
            start.elapsed()
        );
        assert!(err.contains("exceeded"), "{err}");
        assert!(!err.contains("timed out"), "{err}");
    }

    #[test]
    fn clean_stdout_strips_crlf() {
        assert_eq!(clean_stdout(b"hello\r\n"), "hello");
    }

    #[test]
    fn clean_stdout_strips_at_most_one_newline() {
        assert_eq!(clean_stdout(b"hello\n\n"), "hello\n");
    }

    #[test]
    fn clean_stdout_leaves_text_without_trailing_newline_alone() {
        assert_eq!(clean_stdout(b"hello"), "hello");
    }

    #[test]
    fn stdout_outcome_cleans_a_successful_receive() {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        tx.send(b"hi\n".to_vec()).unwrap();
        let result = rx.recv_timeout(Duration::from_millis(10));
        assert_eq!(
            stdout_outcome(result, "some-cmd", b""),
            Ok("hi".to_string())
        );
    }

    #[test]
    fn stdout_outcome_reports_a_timed_out_reader_as_failure() {
        // I3 repro (deterministic half): nothing is ever sent, so the
        // receive times out — this must not collapse to `Ok("")`, since a
        // descendant holding the pipe past the grace window may still have
        // real output in flight.
        let (_tx, rx) = mpsc::channel::<Vec<u8>>();
        let result = rx.recv_timeout(Duration::from_millis(10));
        let err = stdout_outcome(result, "some-cmd", b"").unwrap_err();
        assert!(err.contains("abandoned"), "{err}");
    }

    #[test]
    fn stdout_outcome_reports_a_disconnected_reader_as_failure() {
        // I3 repro (deterministic half): the sender is dropped without
        // sending (standing in for a reader thread that panicked) — this
        // must not collapse to `Ok("")` either.
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        drop(tx);
        let result = rx.recv_timeout(Duration::from_millis(10));
        let err = stdout_outcome(result, "some-cmd", b"").unwrap_err();
        assert!(err.contains("abandoned"), "{err}");
    }

    #[test]
    fn clean_stdout_of_empty_input_is_empty() {
        assert_eq!(clean_stdout(b""), "");
    }
}
