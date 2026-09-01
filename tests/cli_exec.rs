//! Task 5 of the exec() plan: `comb eval` and `comb get` evaluate
//! user-initiated expressions with `.with_exec()` enabled — the CLI user
//! already has a shell, so `exec()` is available to both surfaces.
//!
//! Both tests use the no-daemon harness `tests/cli_eval.rs` and
//! `tests/cli_get_rerouting.rs` establish: a bogus `BEACHCOMBER_SOCKET`
//! proves no daemon is contacted. `exec()` itself makes no daemon-backed
//! reference, so neither test needs a running daemon.

/// `comb eval '{{ exec("sh","-c","echo hi") }}'` runs the command and
/// prints its output.
#[test]
fn eval_of_exec_call_prints_command_output() {
    let dir = tempfile::TempDir::new().unwrap();
    let bogus_sock = dir.path().join("nonexistent.sock");
    let mut cmd = assert_cmd::Command::cargo_bin("comb").unwrap();
    cmd.env("BEACHCOMBER_SOCKET", &bogus_sock)
        .env("RUST_LOG", "error")
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("cfg"))
        .args(["eval", r#"{{ exec("sh", "-c", "echo hi") }}"#]);
    cmd.assert().success().stdout(predicates::str::diff("hi"));
}

/// `comb get demo.shout` resolves a config-declared exec()-backed virtual
/// field client-side (`evaluate_client_side` in `src/cli/commands/get.rs`)
/// — no daemon required, since the field's expression makes no
/// daemon-backed reference either.
#[test]
fn get_of_config_virtual_field_with_exec_prints_command_output() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = dir.path().join("cfg");
    std::fs::create_dir_all(cfg.join("beachcomber")).unwrap();
    std::fs::write(
        cfg.join("beachcomber").join("config.toml"),
        r#"
[providers.demo]
virtual."shout" = '{{ exec("sh", "-c", "echo ok") }}'
"#,
    )
    .unwrap();

    let bogus_sock = dir.path().join("nonexistent.sock");
    let mut cmd = assert_cmd::Command::cargo_bin("comb").unwrap();
    cmd.env("BEACHCOMBER_SOCKET", &bogus_sock)
        .env("RUST_LOG", "error")
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", &cfg)
        .args(["get", "demo.shout"]);
    cmd.assert().success().stdout(predicates::str::diff("ok"));
}
