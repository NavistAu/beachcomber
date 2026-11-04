use beachcomber::daemon;
use std::path::PathBuf;
use tempfile::TempDir;

#[test]
fn is_daemon_responsive_when_no_socket() {
    let path = PathBuf::from("/tmp/nonexistent-beachcomber-test/sock");
    assert!(
        !libbeachcomber::spawn::is_serving(&path),
        "Should return false when socket doesn't exist"
    );
}

/// The env file is the daemon's own environment: a variable it sets reaches a
/// Source's `env.X` and the command its `exec()` runs. Run through a real
/// `comb daemon`, which loads the file before it starts any thread.
#[test]
fn the_env_files_variables_reach_a_sources_env_and_its_exec_child() {
    use crate::common::daemon::TestDaemon;
    use crate::common::socket::IsolatedSocket;

    let socket = IsolatedSocket::new();
    let home = socket.dir.path().join("home");
    let xdg = socket.dir.path().join("xdg");
    let config_dir = xdg.join("beachcomber");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("env"),
        "BC_ENV_FILE_PROBE=from-the-env-file\n",
    )
    .unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        r#"
[daemon.providers.envprobe.sources.s]
data = "{{ {'env': env.BC_ENV_FILE_PROBE, 'child': exec('echo $BC_ENV_FILE_PROBE')} }}"
result.from_env = "{{ data.env }}"
result.from_exec = "{{ data.child }}"
"#,
    )
    .unwrap();
    let daemon = TestDaemon::spawn_in(socket, home, xdg);

    let output = daemon
        .command(&daemon.home)
        .env_remove("BC_ENV_FILE_PROBE")
        .args(["get", "-f", "json", "envprobe"])
        .output()
        .expect("run comb get");
    assert!(output.status.success(), "{output:?}");
    let got: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        got,
        serde_json::json!({
            "from_env": "from-the-env-file",
            "from_exec": "from-the-env-file",
        })
    );
}

/// Setting a process's environment is sound only while it runs one thread,
/// so the env file is `comb daemon`'s to load before its runtime starts. A
/// daemon started inside a running runtime (a test, an embedder) sets nothing.
#[tokio::test]
async fn a_daemon_started_in_a_running_runtime_sets_no_environment_variable() {
    let tmp = TempDir::new().unwrap();
    let env_file = tmp.path().join("env");
    std::fs::write(&env_file, "BC_IN_RUNTIME_PROBE=set\n").unwrap();
    let config = beachcomber::config::Config::from_toml(&format!(
        "[daemon]\nenv_file = {}\n",
        toml::Value::String(env_file.display().to_string())
    ));
    assert!(std::env::var_os("BC_IN_RUNTIME_PROBE").is_none());

    let (handle, ready) = daemon::start_in_process_polling(tmp.path().join("sock"), config);
    ready.await.expect("daemon ready");
    assert_eq!(
        std::env::var_os("BC_IN_RUNTIME_PROBE"),
        None,
        "set from inside the runtime"
    );

    handle.abort();
}
