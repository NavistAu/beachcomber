//! A query command restarts a daemon older than `comb`; a newer one, and
//! any daemon `comb status` finds, is left as it is.
//!
//! The daemon on the socket is a fake: a listener in this process that
//! answers `hello` with the version the test names and `introspect{daemon}`
//! with the pid of a `sleep` it spawned, so the SIGTERM `comb` sends to stop
//! it lands there, not on the test. When the `sleep` dies the listener goes,
//! as a daemon's does. Every other op is refused.

use crate::common::daemon::isolate;
use crate::common::socket::IsolatedSocket;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

const REFUSED: &str = "the fake daemon serves only hello and introspect";

struct FakeDaemon {
    pid: i32,
    /// The `sleep`'s exit, once it has one.
    exited: Option<JoinHandle<ExitStatus>>,
}

impl FakeDaemon {
    fn serve(socket: &Path, version: &'static str) -> Self {
        Self::serve_speaking(socket, version, libbeachcomber::protocol::PROTOCOL_VERSION)
    }

    /// [`Self::serve`], whose `hello` says it speaks `protocol`.
    fn serve_speaking(socket: &Path, version: &'static str, protocol: &'static str) -> Self {
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(socket).unwrap();
        let mut sleep = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = sleep.id() as i32;
        let gone = Arc::new(AtomicBool::new(false));
        let accepting = {
            let gone = Arc::clone(&gone);
            let socket = socket.to_string_lossy().into_owned();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if gone.load(Ordering::SeqCst) {
                        break;
                    }
                    let socket = socket.clone();
                    std::thread::spawn(move || {
                        answer(stream.unwrap(), version, protocol, pid, &socket)
                    });
                }
            })
        };
        let socket = socket.to_path_buf();
        let exited = std::thread::spawn(move || {
            let status = sleep.wait().unwrap();
            gone.store(true, Ordering::SeqCst);
            // Wakes the accept loop, which drops the listener.
            let _ = UnixStream::connect(&socket);
            accepting.join().unwrap();
            status
        });
        Self {
            pid,
            exited: Some(exited),
        }
    }

    fn running(&self) -> bool {
        !self.exited.as_ref().unwrap().is_finished()
    }

    fn exit(&mut self) -> ExitStatus {
        self.exited.take().unwrap().join().unwrap()
    }
}

impl Drop for FakeDaemon {
    fn drop(&mut self) {
        if let Some(exited) = self.exited.take() {
            // SAFETY: the `sleep` is not reaped until `exited` waits on it.
            unsafe { libc::kill(self.pid, libc::SIGKILL) };
            let _ = exited.join();
        }
    }
}

fn answer(stream: UnixStream, version: &str, protocol: &str, pid: i32, socket: &str) {
    let mut writer = stream.try_clone().unwrap();
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { return };
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        let response = match request["op"].as_str() {
            Some("hello") => serde_json::json!({
                "ok": true,
                "data": {"protocol_version": protocol, "daemon_version": version},
            }),
            Some("introspect") if request["subject"] == "daemon" => serde_json::json!({
                "ok": true,
                "data": {
                    "pid": pid, "version": version, "uptime_secs": 1,
                    "socket_path": socket, "requests_total": 0, "in_flight": 0,
                    "active_watchers": 0, "cache_entries": 0,
                    "watch_backend": "native", "verdicts": [],
                },
            }),
            _ => serde_json::json!({"ok": false, "error": REFUSED}),
        };
        if writeln!(writer, "{response}").is_err() {
            return;
        }
    }
}

/// Stops the real daemon a test's `comb` started, which runs detached.
struct Isolated {
    socket: IsolatedSocket,
    home: PathBuf,
    xdg: PathBuf,
}

impl Isolated {
    fn new() -> Self {
        let socket = IsolatedSocket::new();
        let home = socket.dir.path().join("home");
        let xdg = socket.dir.path().join("xdg");
        std::fs::create_dir_all(&home).unwrap();
        Self { socket, home, xdg }
    }

    fn comb(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_comb"));
        isolate(&mut cmd, &self.home, &self.xdg, &self.socket.path)
            .current_dir(&self.home)
            .args(args)
            .output()
            .unwrap()
    }
}

impl Drop for Isolated {
    fn drop(&mut self) {
        let _ = self.comb(&["kill", "--socket", &self.socket.path.to_string_lossy()]);
    }
}

#[test]
fn a_query_restarts_an_older_daemon_and_says_so_on_stderr() {
    let env = Isolated::new();
    let mut fake = FakeDaemon::serve(&env.socket.path, "0.0.1");

    let out = env.comb(&["get", "env.HOME"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr: {stderr}");
    // Before the exit is waited for: a fake nobody stopped never has one.
    assert!(
        stderr.contains(&format!(
            "restarted daemon 0.0.1 -> {}",
            libbeachcomber::VERSION
        )),
        "stderr: {stderr}"
    );
    assert_eq!(
        fake.exit().signal(),
        Some(libc::SIGTERM),
        "stopped as comb kill stops a daemon"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        env.home.to_string_lossy(),
        "stdout has the value alone"
    );
    let hello = crate::common::socket::ask(&env.socket.path, serde_json::json!({"op": "hello"}));
    assert_eq!(
        hello["data"]["daemon_version"],
        libbeachcomber::VERSION,
        "a daemon of this comb's own release serves the socket now"
    );
}

#[test]
fn a_query_leaves_a_newer_daemon_alone() {
    let env = Isolated::new();
    let fake = FakeDaemon::serve(&env.socket.path, "999.0.0");

    // The fake refuses the `put`: the command was served by it, not a new one.
    let out = env.comb(&["put", "x.y", "1"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(REFUSED), "stderr: {stderr}");
    assert!(!stderr.contains("restarted daemon"), "stderr: {stderr}");
    assert!(fake.running(), "the newer daemon was not stopped");
}

#[test]
fn status_warns_of_an_older_daemon_and_leaves_it() {
    let env = Isolated::new();
    let fake = FakeDaemon::serve(&env.socket.path, "0.0.1");

    let out = env.comb(&["status"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&format!(
            "daemon 0.0.1 is older than comb {}; the next query restarts it",
            libbeachcomber::VERSION
        )),
        "stderr: {stderr}"
    );
    assert!(!stderr.contains("restarted daemon"), "stderr: {stderr}");
    assert!(fake.running(), "status did not stop the daemon");
}

/// A daemon of another protocol major is refused, by every query: comb
/// names both versions and does not send it the op.
#[test]
fn a_daemon_of_another_protocol_major_is_refused() {
    let env = Isolated::new();
    let fake = FakeDaemon::serve_speaking(&env.socket.path, "999.0.0", "3.0");
    for args in [&["put", "x.y", "1"][..], &["get", "x.y"], &["refresh", "x"]] {
        let out = env.comb(args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{args:?}: {stderr}");
        assert!(stderr.contains("speaks protocol 3.0"), "{args:?}: {stderr}");
        assert!(!stderr.contains(REFUSED), "never sent the op: {stderr}");
    }
    assert!(fake.running());
}
