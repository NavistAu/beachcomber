//! `--socket`, the global flag that names the daemon's socket: it beats
//! `[daemon] socket_path` and `$BEACHCOMBER_SOCKET`, and reads the same on
//! every command, `daemon` and `kill` included.

use crate::common::daemon::TestDaemon;

/// With `$BEACHCOMBER_SOCKET` pointing nowhere, `--socket` reaches the
/// daemon, before or after the subcommand.
#[test]
fn the_flag_beats_the_environment_on_every_command() {
    let d = TestDaemon::spawn();
    let sock = d.socket.path.to_str().unwrap().to_string();
    let elsewhere = d.home.join("elsewhere.sock");

    d.comb(&d.home)
        .env("BEACHCOMBER_SOCKET", &elsewhere)
        .args(["--socket", &sock, "put", "sk", r#"{"v":1}"#, "--global"])
        .assert()
        .success();
    let out = d
        .comb(&d.home)
        .env("BEACHCOMBER_SOCKET", &elsewhere)
        .args(["get", "sk.v", "--global", "--socket", &sock])
        .assert()
        .success();
    assert_eq!(String::from_utf8_lossy(&out.get_output().stdout), "1");
    assert!(
        !elsewhere.exists(),
        "nothing was started at the environment's socket"
    );
}

/// `kill` takes the same flag, and stops the daemon it names through that
/// daemon's own report of its pid.
#[test]
fn kill_takes_the_global_flag() {
    let d = TestDaemon::spawn();
    let sock = d.socket.path.to_str().unwrap().to_string();
    let out = d
        .comb(&d.home)
        .env_remove("BEACHCOMBER_SOCKET")
        .args(["kill", "--socket", &sock])
        .assert()
        .success();
    assert!(
        String::from_utf8_lossy(&out.get_output().stdout).starts_with("Daemon stopped (pid "),
        "{:?}",
        out.get_output()
    );
}
