//! Starting a daemon: `libbeachcomber::spawn::ensure`, the one spawner the CLI
//! and every SDK use, and `spawn::launch` under it. A stand-in `comb` — a
//! shell script — shows what a spawn runs, where its stderr goes and what the
//! spawner makes of its readiness pipe, without a daemon ever binding; the
//! real `comb daemon` shows it writes that pipe when it serves.

use crate::common::daemon::{TestDaemon, isolate};
use crate::common::socket::IsolatedSocket;
use libbeachcomber::spawn::{self, Error, Spawn, ensure, launch};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A stand-in `comb` in `dir` running `body` after it has written its
/// arguments to `args` and set `$fd` to the number `--ready-fd` names.
fn stand_in(dir: &Path, body: &str) -> (PathBuf, PathBuf) {
    let args = dir.join("args");
    let bin = dir.join("comb");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\necho \"$@\" > {}\nwhile [ $# -gt 0 ]; do [ \"$1\" = --ready-fd ] && fd=$2; shift; done\n{body}\n",
            args.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    (bin, args)
}

/// A stand-in that prints `early-error` on stderr and exits without binding
/// or writing its readiness pipe.
fn refusing(dir: &Path) -> (PathBuf, PathBuf) {
    stand_in(dir, "echo early-error >&2\nexit 2")
}

/// A daemon already serving the socket is used as it is: nothing is run.
#[test]
fn a_serving_socket_is_used_without_spawning() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("sock");
    let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    let (bin, args) = refusing(tmp.path());
    let log = tmp.path().join("daemon.log");

    let spawn = Spawn {
        binary: &bin,
        no_reap: false,
        log: &log,
    };
    ensure(&sock, &spawn).expect("the serving daemon");
    assert!(!args.exists(), "nothing was spawned");
}

/// A socket path the kernel cannot bind is refused before anything runs.
#[test]
fn an_overlong_socket_is_refused_without_spawning() {
    let tmp = tempfile::tempdir().unwrap();
    let long = PathBuf::from(format!("/tmp/{}/sock", "x".repeat(120)));
    let (bin, args) = refusing(tmp.path());
    let log = tmp.path().join("daemon.log");

    let spawn = Spawn {
        binary: &bin,
        no_reap: false,
        log: &log,
    };
    let err = ensure(&long, &spawn).expect_err("refused");
    assert!(matches!(err, Error::Socket(_)), "{err:?}");
    assert!(err.to_string().contains("SUN_LEN"), "{err}");
    assert!(!args.exists(), "nothing was spawned");
}

/// A spawn runs `comb daemon --socket <socket>`, `--no-reap` when asked,
/// with a readiness pipe (`--ready-fd`), and appends its stderr to the
/// daemon's log — so what it prints before its own logger starts is kept. A
/// daemon that exits without saying it serves is reported the moment it
/// exits, with its status and what it said, not after a timeout.
#[test]
fn a_daemon_that_exits_before_serving_is_reported_at_once_with_what_it_said() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("run").join("sock");
    let (bin, args) = refusing(tmp.path());
    let log = tmp.path().join("state").join("daemon.log");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(&log, "an earlier daemon's line\n").unwrap();

    let spawn = Spawn {
        binary: &bin,
        no_reap: true,
        log: &log,
    };
    let started = Instant::now();
    let err = ensure(&sock, &spawn).expect_err("the stand-in exits");
    let took = started.elapsed();
    match &err {
        Error::Exited { status, said } => {
            assert_eq!(status.code(), Some(2), "{err}");
            assert_eq!(said.as_deref(), Some("early-error"), "{err}");
        }
        other => panic!("expected Exited, got {other:?}"),
    }
    assert!(
        took < spawn::READY_TIMEOUT / 2,
        "an exit is seen at once, not after a wait: {took:?}"
    );
    assert_eq!(
        err.to_string(),
        "exited before serving (exit status: 2): early-error"
    );

    let argv = std::fs::read_to_string(&args).unwrap();
    let prefix = format!("daemon --socket {} --no-reap --ready-fd ", sock.display());
    let fd = argv
        .trim()
        .strip_prefix(&prefix)
        .unwrap_or_else(|| panic!("{argv:?} starts {prefix:?}"));
    assert!(fd.parse::<u32>().is_ok_and(|fd| fd > 2), "{argv:?}");
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(logged.contains("early-error"), "{logged:?}");
    assert!(
        sock.parent().unwrap().is_dir(),
        "the socket's directory is made"
    );
}

/// The spawner takes the daemon's word: the moment the stand-in writes its
/// readiness pipe, `ensure` returns, though nothing is bound — it never polls
/// the socket.
#[test]
fn a_daemon_is_taken_to_serve_the_moment_it_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("sock");
    let (bin, _) = stand_in(tmp.path(), "eval \"echo >&$fd\"\nexit 0");
    let log = tmp.path().join("daemon.log");

    let spawn = Spawn {
        binary: &bin,
        no_reap: false,
        log: &log,
    };
    ensure(&sock, &spawn).expect("the stand-in said it serves");
    assert!(!sock.exists(), "nothing was bound");
}

/// A daemon that neither says it serves nor exits is a timeout once the wait
/// is up; it is left to run.
#[test]
fn a_daemon_that_neither_serves_nor_exits_is_a_timeout() {
    let tmp = tempfile::tempdir().unwrap();
    // `exec sleep` keeps the readiness pipe open, unwritten, for a while
    // after the wait is up, then exits on its own.
    let (bin, _) = stand_in(tmp.path(), "exec sleep 1");
    let mut cmd = Command::new(&bin);
    cmd.stdout(Stdio::null()).stderr(Stdio::null());

    let wait = Duration::from_millis(50);
    let err = launch(&mut cmd, None, wait).expect_err("it never says it serves");
    assert!(matches!(err, Error::TimedOut(w) if w == wait), "{err:?}");
}

/// The real daemon writes its readiness pipe once its socket is bound: a
/// launch returns with the socket already served.
#[test]
fn the_daemon_says_it_serves_once_its_socket_is_bound() {
    let socket = IsolatedSocket::new();
    let home = socket.dir.path().join("home");
    let xdg = socket.dir.path().join("xdg");
    std::fs::create_dir_all(&home).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_comb"));
    isolate(&mut cmd, &home, &xdg, &socket.path)
        .args(["daemon", "--exit-with-parent", "--socket"])
        .arg(&socket.path);

    let mut child = launch(&mut cmd, None, spawn::READY_TIMEOUT).expect("the daemon serves");
    let serving = spawn::is_serving(&socket.path);
    crate::common::daemon::stop(&mut child);
    assert!(serving, "served by the time it said so");
}

/// A daemon that finds a daemon of its own build already serving its socket
/// exits, and says the socket is served: the spawner's daemon is there.
#[test]
fn a_daemon_that_finds_its_socket_served_says_so_and_exits() {
    let first = TestDaemon::spawn();
    let mut cmd = first.command(first.home.as_path());
    cmd.args(["daemon", "--socket"]).arg(&first.socket.path);

    let mut second = launch(&mut cmd, None, spawn::READY_TIMEOUT).expect("the socket is served");
    let status = second.wait().expect("it exits");
    assert!(status.success(), "{status}");
}

/// A daemon that refuses its configuration says why on its stderr and closes
/// its readiness pipe unwritten; the launch reports the refusal from the log.
#[test]
fn a_daemon_that_refuses_its_configuration_is_reported_with_its_first_error() {
    let socket = IsolatedSocket::new();
    let home = socket.dir.path().join("home");
    let xdg = socket.dir.path().join("xdg");
    std::fs::create_dir_all(xdg.join("beachcomber")).unwrap();
    std::fs::write(
        xdg.join("beachcomber/config.toml"),
        "[providers.conda]\nenv = \"x\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let log = home.join("daemon.log");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_comb"));
    isolate(&mut cmd, &home, &xdg, &socket.path)
        .args(["daemon", "--exit-with-parent", "--socket"])
        .arg(&socket.path);

    let err = launch(&mut cmd, Some(&log), spawn::READY_TIMEOUT).expect_err("refused");
    match &err {
        Error::Exited { status, said } => {
            assert_eq!(status.code(), Some(2), "{err}");
            let said = said.as_deref().unwrap_or_default();
            assert!(
                said.starts_with("error: ") && said.contains("unknown table [providers]"),
                "the first error, not the verdict after it: {err}"
            );
        }
        other => panic!("expected Exited, got {other:?}"),
    }
    assert!(!socket.path.exists(), "it never bound");
}

/// A test's daemon is stopped as `comb kill` stops one, by SIGTERM, so it
/// shuts down cleanly — and an instrumented build writes its coverage
/// profile — rather than being SIGKILLed.
#[test]
fn a_test_daemon_is_stopped_cleanly() {
    let outside = tempfile::tempdir().unwrap();
    let home = outside.path().join("home");
    let xdg = outside.path().join("xdg");
    drop(TestDaemon::spawn_in(
        IsolatedSocket::new(),
        home.clone(),
        xdg,
    ));

    let log = std::fs::read_to_string(home.join(".local/state/beachcomber/daemon.log"))
        .expect("the daemon's log");
    assert!(log.contains("Received SIGTERM"), "{log}");
    assert!(log.contains("Daemon shut down cleanly"), "{log}");
}

/// A daemon out of descriptors logs that once, whatever keeps knocking, and
/// serves again as soon as one is free. Every failed accept logged a line of
/// its own, and on Linux, where a pending connection stays pending, the loop
/// tried again at once for as long as it lasted.
#[test]
fn a_daemon_out_of_descriptors_logs_once_and_recovers() {
    use std::os::unix::process::CommandExt;
    const LIMIT: u64 = 32;

    let socket = IsolatedSocket::new();
    let home = socket.dir.path().join("home");
    let xdg = socket.dir.path().join("xdg");
    std::fs::create_dir_all(&home).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_comb"));
    isolate(&mut cmd, &home, &xdg, &socket.path)
        .env("RUST_LOG", "info")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .args(["daemon", "--exit-with-parent", "--socket"])
        .arg(&socket.path);
    // SAFETY: `setrlimit` is async-signal-safe and touches only this child.
    unsafe {
        cmd.pre_exec(|| {
            let limit = libc::rlimit {
                rlim_cur: LIMIT,
                rlim_max: LIMIT,
            };
            if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let mut child = launch(&mut cmd, None, spawn::READY_TIMEOUT).expect("the daemon serves");

    // A connection the daemon answers, or the last that it does not.
    let answered = || {
        let mut stream = std::os::unix::net::UnixStream::connect(&socket.path).ok()?;
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        stream.write_all(b"{\"op\":\"hello\"}\n").ok()?;
        matches!(stream.read(&mut [0u8; 1]), Ok(1)).then_some(stream)
    };
    // Hold connections until the daemon can take no more.
    let mut held = Vec::new();
    while let Some(stream) = answered() {
        assert!(held.len() < LIMIT as usize, "the limit was never reached");
        held.push(stream);
    }
    // More knocking, each a failure of its own while none is free.
    let knocks: Vec<_> = (0..8)
        .map(|_| std::os::unix::net::UnixStream::connect(&socket.path))
        .collect();
    let log = home.join(".local/state/beachcomber/daemon.log");
    // Failed runs logged and runs recovered from, read at one instant.
    let runs = || {
        let log = std::fs::read_to_string(&log).unwrap_or_default();
        let count = |what| log.lines().filter(|l| l.contains(what)).count();
        (count("Accept error"), count("Accepting connections again"))
    };
    use crate::common::poll::poll_for;
    assert!(
        poll_for(Duration::from_secs(5), Duration::from_millis(5), || {
            runs().0 > 0
        }),
        "it ran out of descriptors"
    );
    // Nothing to wait on but the absence of more: long enough for the knocks,
    // and for a loop that did not back off to have logged thousands. A descriptor
    // the daemon frees for a moment lets one knock in and starts a new run.
    let more = poll_for(Duration::from_millis(250), Duration::from_millis(5), || {
        let (failed, recovered) = runs();
        failed > recovered + 1
    });
    assert!(!more, "one line for each run of failures: {:?}", runs());

    drop(knocks);
    drop(held);
    let recovered = poll_for(Duration::from_secs(5), Duration::from_millis(5), || {
        answered().is_some()
    });
    crate::common::daemon::stop(&mut child);
    assert!(recovered, "it serves again once descriptors are free");
}
