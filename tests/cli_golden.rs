mod common;
use common::daemon::TestDaemon;
use std::time::Duration;

#[test]
fn daemon_fixture_starts_and_exits() {
    let d = TestDaemon::spawn();
    assert!(d.socket.path.exists());
    drop(d);
}

// ── helper ────────────────────────────────────────────────────────────────────

/// Build a `comb` Command pre-configured with `BEACHCOMBER_SOCKET` pointing
/// directly at the test daemon's socket path, so `Config::resolve_socket_path()`
/// resolves to the isolated test socket.
///
/// The current_dir is fixed to "/" so that path-scoped virtual keys written with
/// `--path /` are found by `get` (which always injects CWD as path context for
/// virtual providers).
fn comb(d: &TestDaemon) -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("comb").unwrap();
    cmd.env("BEACHCOMBER_SOCKET", &d.socket.path);
    // Suppress daemon-start output from bleeding into stdout assertions.
    cmd.env("RUST_LOG", "error");
    // Fix CWD to "/" so `get` injects a stable, known path context. Virtual
    // keys must be stored with `--path /` (or `--path .` in this CWD).
    cmd.current_dir("/");
    cmd
}

// ── golden_get_returns_known_value ────────────────────────────────────────────

#[test]
fn golden_get_returns_known_value() {
    let d = TestDaemon::spawn();

    // Seed a virtual key.  `get` always injects CWD as path context for virtual
    // providers; we fix CWD to "/" (see `comb()` helper) so we must also put
    // with `--path /` to store under the same cache key.
    comb(&d)
        .args(["put", "myapp", r#"{"status":"healthy"}"#, "--path", "/"])
        .assert()
        .success();

    comb(&d)
        .args(["get", "myapp.status"])
        .assert()
        .success()
        .stdout(predicates::str::contains("healthy"));
}

// ── golden_status_shows_running_daemon ────────────────────────────────────────

#[test]
fn golden_status_shows_running_daemon() {
    let d = TestDaemon::spawn();

    // `comb status` talks to the daemon and prints cache rows or column headers.
    // With an empty cache it may print nothing, but it must exit 0.
    comb(&d).args(["status"]).assert().success();
}

// ── golden_put_sets_a_value ───────────────────────────────────────────────────

#[test]
fn golden_put_sets_a_value() {
    let d = TestDaemon::spawn();

    // put exits 0; a subsequent get returns the stored value.
    // `--path /` matches the "/" CWD injected by get's path resolution.
    comb(&d)
        .args(["put", "testprov", r#"{"field":"golden"}"#, "--path", "/"])
        .assert()
        .success();

    comb(&d)
        .args(["get", "testprov.field"])
        .assert()
        .success()
        .stdout(predicates::str::contains("golden"));
}

// ── golden_watch_emits_initial_value_then_exits ───────────────────────────────

#[test]
fn golden_watch_emits_value_then_exits() {
    let d = TestDaemon::spawn();

    // Seed a value so the watch has something to emit immediately.
    comb(&d)
        .args(["put", "watchprov", r#"{"val":"stream-me"}"#, "--path", "/"])
        .assert()
        .success();

    // `watch` streams forever; kill it after 2 s.  The initial value line is
    // written before the process is killed, so stdout will contain it even
    // though the exit code is non-zero (killed by timeout).
    // `--path /` matches the `--path /` used in the put above.
    comb(&d)
        .args(["watch", "watchprov.val", "--path", "/"])
        .timeout(Duration::from_secs(2))
        .assert()
        .stdout(predicates::str::contains("stream-me"));
}

// ── golden_eval_renders_template ─────────────────────────────────────────────

#[test]
fn golden_eval_renders_template() {
    let d = TestDaemon::spawn();

    // A template with no provider.field references is rendered directly without
    // touching the daemon — cheapest golden path.
    comb(&d)
        .args(["eval", "hello world"])
        .assert()
        .success()
        .stdout(predicates::str::contains("hello world"));
}

// ── golden_check_daemon ───────────────────────────────────────────────────────

#[test]
fn golden_check_runs_diagnostics() {
    let d = TestDaemon::spawn();

    // `comb check daemon` contacts the running daemon and prints PASS/INFO lines.
    // Exit code is 0 when the daemon responds cleanly.
    comb(&d)
        .args(["check", "daemon"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Daemon"));
}

// ── golden_kill_terminates_daemon ─────────────────────────────────────────────

#[test]
fn golden_kill_terminates_daemon() {
    let d = TestDaemon::spawn();

    // `kill` needs --socket because it derives the pid file path from it.
    // The socket flag is only accepted on the Kill subcommand, not at top level.
    assert_cmd::Command::cargo_bin("comb")
        .unwrap()
        .env("BEACHCOMBER_SOCKET", &d.socket.path)
        .env("RUST_LOG", "error")
        .args(["kill", "--socket"])
        .arg(&d.socket.path)
        .assert()
        .success();

    // After kill the socket should no longer accept connections.
    assert!(
        !d.socket.path.exists() || {
            use std::os::unix::net::UnixStream;
            UnixStream::connect(&d.socket.path).is_err()
        }
    );
}

// ── golden_init_prints_or_writes_config ──────────────────────────────────────

#[test]
fn golden_init_prints_or_writes_config() {
    let d = TestDaemon::spawn();

    // `comb init` detects installed tools and prints suggestions.
    // It doesn't contact the daemon, so it always exits 0.
    comb(&d).args(["init"]).assert().success();
}
