//! Starting a daemon: `libbeachcomber::spawn::ensure`, the one spawner the CLI
//! and every SDK use. A stand-in `comb` — a shell script — shows what a spawn
//! runs and where its stderr goes, without a daemon ever binding.

use libbeachcomber::spawn::{Spawn, ensure};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// A stand-in `comb` in `dir`: it writes its arguments to `args`, prints
/// `early-error` on stderr, and exits without binding anything.
fn stand_in(dir: &Path) -> (PathBuf, PathBuf) {
    let args = dir.join("args");
    let bin = dir.join("comb");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\necho \"$@\" > {}\necho early-error >&2\nexit 2\n",
            args.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    (bin, args)
}

/// A daemon already serving the socket is used as it is: nothing is run.
#[test]
fn a_serving_socket_is_used_without_spawning() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("sock");
    let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    let (bin, args) = stand_in(tmp.path());
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
    let (bin, args) = stand_in(tmp.path());
    let log = tmp.path().join("daemon.log");

    let spawn = Spawn {
        binary: &bin,
        no_reap: false,
        log: &log,
    };
    let err = ensure(&long, &spawn).expect_err("refused");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("SUN_LEN"), "{err}");
    assert!(!args.exists(), "nothing was spawned");
}

/// A spawn runs `comb daemon --socket <socket>`, `--no-reap` when asked,
/// appends its stderr to the daemon's log — so what it prints before its own
/// logger starts is kept — and a daemon that never binds is a timeout.
#[test]
fn a_spawn_runs_the_daemon_with_its_stderr_in_the_log() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("run").join("sock");
    let (bin, args) = stand_in(tmp.path());
    let log = tmp.path().join("state").join("daemon.log");

    let spawn = Spawn {
        binary: &bin,
        no_reap: true,
        log: &log,
    };
    let err = ensure(&sock, &spawn).expect_err("the stand-in never binds");
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);

    let argv = std::fs::read_to_string(&args).unwrap();
    assert_eq!(
        argv.trim(),
        format!("daemon --socket {} --no-reap", sock.display())
    );
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(logged.contains("early-error"), "{logged:?}");
    assert!(
        sock.parent().unwrap().is_dir(),
        "the socket's directory is made"
    );
}
