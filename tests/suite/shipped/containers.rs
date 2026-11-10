//! `defaults/docker.toml`, `podman.toml` and `containers.toml`: client templates
//! over stand-in `docker` and `podman` on a temp `PATH`; no real engine is
//! called. A stand-in answers `ps -q` with the ids in `$STUB_IDS` (docker) or
//! `$P_IDS` (podman) and exits `$STUB_EXIT` / `$P_EXIT`, and logs each `ps`
//! to `$STUB_LOG`.
//!
//! Also the `which` gates the shipped `kubecontext` and `talos` templates start with.

use super::{Scenario, scenario};
use serde_json::{Value, json};
use std::path::PathBuf;

struct Env {
    s: Scenario,
    bin: PathBuf,
    log: PathBuf,
}

/// A scenario, and a bin directory beside its home holding stand-ins for
/// the `names`. `get`'s `PATH` is that bin, then the system's.
fn env_with(names: &[&str]) -> Env {
    // SAFETY: nextest runs each test in its own process, before any daemon.
    unsafe {
        std::env::set_var(
            "PATH",
            crate::common::daemon::system_bin().display().to_string(),
        )
    };
    let s = scenario();
    let base = s.daemon.home.parent().unwrap().to_path_buf();
    let bin = base.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for name in names {
        let body = match *name {
            "docker" => {
                "case \"$1\" in\n context) echo \"${DOCKER_CONTEXT:-orbstack}\" ;;\n ps) echo \"docker $*\" >> \"$STUB_LOG\"; printf '%s' \"$STUB_IDS\"; exit ${STUB_EXIT:-0} ;;\nesac"
            }
            "podman" => {
                "case \"$1\" in\n system) echo '[{\"Name\":\"other\",\"Default\":false},{\"Name\":\"podman-machine-default\",\"Default\":true}]' ;;\n ps) echo \"podman $*\" >> \"$STUB_LOG\"; printf '%s' \"$P_IDS\"; exit ${P_EXIT:-0} ;;\nesac"
            }
            "kubectl" => "echo \"kubectl $*\" >> \"$STUB_LOG\"; printf ctx-1",
            "talosctl" => {
                "echo \"talosctl $*\" >> \"$STUB_LOG\"; printf '{\"context\":\"admin@talos\"}'"
            }
            other => panic!("no stand-in for {other}"),
        };
        crate::common::daemon::tool(&bin, name, &format!("#!/bin/sh\n{body}\n"));
    }
    Env {
        log: base.join("stub.log"),
        s,
        bin,
    }
}

impl Env {
    fn get(&self, key: &str, vars: &[(&str, &str)]) -> Value {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_comb"));
        cmd.env_clear();
        crate::common::daemon::isolate(
            &mut cmd,
            &self.s.daemon.home,
            &self.s.daemon.xdg,
            &self.s.daemon.socket.path,
        );
        // Only the stand-ins and the system: not the stub bin the other tests use.
        cmd.env(
            "PATH",
            format!(
                "{}:{}",
                self.bin.display(),
                crate::common::daemon::system_bin().display()
            ),
        )
        .env("STUB_LOG", &self.log)
        .envs(vars.iter().copied());
        let out = cmd
            .current_dir(&self.s.daemon.home)
            .args(["get", "-f", "json", key])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "get {key}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn runs(&self) -> usize {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .count()
    }
}

/// Every state of the engine, one read of the whole provider each. A memo is
/// keyed on the shell's `DOCKER_*` variables, not on a stand-in's, so each
/// case names a `DOCKER_CONFIG` of its own (it moves only the watched file):
/// the cases share one daemon and stay apart. A stopped engine (`ps` fails)
/// is not running and has no count; the context follows `DOCKER_CONTEXT`.
#[test]
fn docker_reads_every_state_of_the_engine() {
    let e = env_with(&["docker"]);
    for (i, (vars, want)) in [
        (
            vec![("STUB_IDS", "a1\nb2\nc3\n")],
            json!({"context": "orbstack", "count": 3, "running": true}),
        ),
        (
            vec![("STUB_IDS", "")],
            json!({"context": "orbstack", "count": 0, "running": true}),
        ),
        (
            vec![("STUB_EXIT", "1")],
            json!({"context": "orbstack", "running": false}),
        ),
        (
            vec![("DOCKER_CONTEXT", "work")],
            json!({"context": "work", "count": 0, "running": true}),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let config = format!("/case-{i}");
        let v: Vec<(&str, &str)> = vars
            .iter()
            .copied()
            .chain([("DOCKER_CONFIG", config.as_str())])
            .collect();
        assert_eq!(e.get("docker", &v), want, "{vars:?}");
    }
    assert_eq!(e.get("docker.context", &[]), json!("orbstack"));
}

/// One `ps` serves both fields of a read.
#[test]
fn docker_ps_runs_once_per_read() {
    let e = env_with(&["docker"]);
    assert_eq!(
        e.get("docker", &[("STUB_IDS", "a\nb\n")]),
        json!({"context": "orbstack", "count": 2, "running": true})
    );
    assert_eq!(e.runs(), 1);
    let e = env_with(&["podman"]);
    e.get("podman", &[("P_IDS", "x\n")]);
    assert_eq!(e.runs(), 1);
}

/// Memoised: many reads, one `ps`.
#[test]
fn docker_ps_runs_once_for_many_reads() {
    let e = env_with(&["docker"]);
    let v = [("STUB_IDS", "a\n")];
    for _ in 0..3 {
        e.get("docker.running", &v);
        e.get("docker.count", &v);
    }
    assert_eq!(e.runs(), 1);
}

/// No `docker` on the PATH: everything is absent, nothing spawned.
#[test]
fn docker_missing_is_absent() {
    let e = env_with(&[]);
    for k in ["docker.context", "docker.running", "docker.count"] {
        assert_eq!(e.get(k, &[]), Value::Null, "{k}");
    }
    assert_eq!(e.runs(), 0);
}

/// The connection is podman's default, or the shell's
/// `CONTAINER_CONNECTION`; a down engine is not running. Each case names a
/// `CONTAINER_HOST` of its own, a key of the memo, so they share a daemon.
#[test]
fn podman_reads_its_connection_and_engine() {
    let e = env_with(&["podman"]);
    assert_eq!(
        e.get(
            "podman",
            &[("P_IDS", "x\ny\n"), ("CONTAINER_HOST", "/case-0")]
        ),
        json!({"context": "podman-machine-default", "count": 2, "running": true})
    );
    let v = [
        ("CONTAINER_CONNECTION", "mine"),
        ("P_EXIT", "125"),
        ("CONTAINER_HOST", "/case-1"),
    ];
    assert_eq!(
        e.get("podman", &v),
        json!({"context": "mine", "running": false})
    );
}

#[test]
fn podman_missing_is_absent() {
    let e = env_with(&["docker"]);
    for k in ["podman.context", "podman.running", "podman.count"] {
        assert_eq!(e.get(k, &[]), Value::Null, "{k}");
    }
}

/// `containers`: podman first, counts summed, `running: false` when both are
/// installed and down.
#[test]
fn containers_reads_both_engines() {
    // A memo is keyed on the shell's environment, not on a stand-in's, so each
    // case names its own `DOCKER_CONFIG` and `CONTAINER_HOST`.
    let e = env_with(&["docker", "podman"]);
    let case = |n: &str, vars: &[(&str, &str)]| {
        let v: Vec<(&str, &str)> = vars
            .iter()
            .copied()
            .chain([("DOCKER_CONFIG", n), ("CONTAINER_HOST", n)])
            .collect();
        e.get("containers", &v)
    };
    assert_eq!(
        case("/case-0", &[("STUB_IDS", "a\nb\nc\n"), ("P_IDS", "x\n")]),
        json!({"engine": "podman", "running": true, "count": 4})
    );
    assert_eq!(
        case("/case-1", &[("STUB_IDS", "a\nb\n"), ("P_EXIT", "125")]),
        json!({"engine": "docker", "running": true, "count": 2})
    );
    assert_eq!(
        case("/case-2", &[("STUB_EXIT", "1"), ("P_EXIT", "125")]),
        json!({"running": false})
    );
}

#[test]
fn containers_is_absent_with_neither_tool() {
    let e = env_with(&[]);
    assert_eq!(e.get("containers", &[]), Value::Null);
}

#[test]
fn containers_with_one_tool_installed() {
    let e = env_with(&["podman"]);
    assert_eq!(
        e.get("containers", &[("P_IDS", "x\n")]),
        json!({"engine": "podman", "running": true, "count": 1})
    );
}

/// The `kubecontext` and `talos` gates: without the tool on the PATH a read is
/// absent and nothing is spawned; with it, the context.
#[test]
fn kubecontext_and_talos_are_gated_on_the_tool() {
    let e = env_with(&[]);
    for k in [
        "kubecontext.context",
        "kubecontext.namespace",
        "talos.context",
    ] {
        assert_eq!(e.get(k, &[]), Value::Null, "{k}");
    }
    assert_eq!(e.runs(), 0);
    let e = env_with(&["kubectl", "talosctl"]);
    assert_eq!(e.get("kubecontext.context", &[]), json!("ctx-1"));
    assert_eq!(e.get("talos.context", &[]), json!("admin@talos"));
}
