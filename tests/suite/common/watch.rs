//! `comb watch` as a child: it streams forever, so a test reads the lines it
//! wants on a background thread bounded by `recv_timeout` (no sleeps — see
//! `tests/suite/no_sleep_in_tests.rs`), then kills it, rather than wait out
//! a timeout that kills it for the test.

use crate::common::daemon::TestDaemon;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// A running `comb watch` child, killed and reaped on drop so an early
/// `assert!` failure in a test body never leaks a daemon-attached process.
pub struct WatchChild(pub Child);

impl Drop for WatchChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Spawn `comb watch <args>` at `cwd`, isolated as [`TestDaemon::command`]
/// isolates it, with piped stdout and stderr.
#[allow(dead_code)]
pub fn spawn_watch(d: &TestDaemon, cwd: &Path, args: &[&str]) -> WatchChild {
    let mut cmd = d.command(cwd);
    cmd.arg("watch")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    WatchChild(cmd.spawn().expect("spawn comb watch"))
}

/// A running `comb watch` whose stdout lines arrive on a channel fed by a
/// background thread, so a test can wait for one frame, provoke the next,
/// and wait again — all on bounded `recv_timeout`s, never a sleep.
pub struct WatchLines {
    child: WatchChild,
    rx: mpsc::Receiver<String>,
}

/// Spawn `comb watch <args>` at `cwd` and start draining its stdout.
#[allow(dead_code)]
pub fn watch_lines(d: &TestDaemon, cwd: &Path, args: &[&str]) -> WatchLines {
    let mut child = spawn_watch(d, cwd, args);
    let stdout = child.0.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { return };
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    WatchLines { child, rx }
}

impl WatchLines {
    /// The next frame's line, waiting up to 5s (the suite's norm for
    /// spawn+connect+first-frame waits). A timeout panics naming what the
    /// child wrote to stderr rather than failing as a bare `None`.
    pub fn next_line(&mut self) -> String {
        match self.rx.recv_timeout(Duration::from_secs(5)) {
            Ok(line) => line,
            Err(_) => panic!(
                "comb watch produced no further output within 5s; stderr:\n{}",
                self.stderr()
            ),
        }
    }

    pub fn stderr(&mut self) -> String {
        let _ = self.child.0.kill();
        let mut stderr = String::new();
        if let Some(mut s) = self.child.0.stderr.take() {
            let _ = s.read_to_string(&mut stderr);
        }
        let _ = self.child.0.wait();
        stderr
    }
}

/// The first frame of `comb watch <args>` at `cwd`, then the child is killed.
#[allow(dead_code)]
pub fn first_line(d: &TestDaemon, cwd: &Path, args: &[&str]) -> String {
    watch_lines(d, cwd, args).next_line()
}
