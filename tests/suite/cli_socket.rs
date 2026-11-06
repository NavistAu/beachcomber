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

/// With no daemon running, a configuration the daemon would refuse is
/// reported by the command itself, in the daemon's words, before anything is
/// spawned — not as a daemon that times out starting.
#[test]
fn a_refused_config_is_reported_before_spawning() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let xdg = tmp.path().join("xdg");
    std::fs::create_dir_all(xdg.join("beachcomber")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        xdg.join("beachcomber").join("config.toml"),
        "[daemon]\nlog_level = \"loud\"\n",
    )
    .unwrap();
    let sock = tmp.path().join("sock");

    let started = std::time::Instant::now();
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_comb"));
    crate::common::daemon::isolate(&mut cmd, &home, &xdg, &sock);
    let out = cmd.args(["get", "x.v"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("log_level"), "{stderr}");
    assert!(
        stderr.contains("error: daemon: refusing to start: the configuration has 1 error(s)"),
        "{stderr}"
    );
    assert!(!sock.exists(), "nothing was spawned");
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
}
