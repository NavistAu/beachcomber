//! The shipped files, read as a user reads them: one module per shipped
//! namespace (`shipped/<namespace>.rs`), each through [`scenario`].
//!
//! A scenario's daemon runs in this test's process over a temp `HOME` and
//! `XDG_CONFIG_HOME` and writes the managed defaults as `comb daemon` does, so
//! the file under test is the real shipped one, read from `defaults/`. A test
//! sets up what the provider reads in [`Scenario::project`] (a repository, a
//! `.tool-versions`, a `pyvenv.cfg`), asks [`Scenario::get`], and asserts on
//! the JSON. Where a value depends on the machine (the hostname, the load) it
//! asserts the shape and the type.

use crate::common::daemon::TestDaemon;
use crate::common::socket::IsolatedSocket;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

mod battery;
mod hostname;
mod load;
mod network;
mod uname;
mod uptime;
mod user;

/// A daemon on the shipped defaults, and a directory to read from.
pub struct Scenario {
    pub daemon: TestDaemon,
    /// An empty directory under the daemon's `HOME`, canonical.
    pub project: PathBuf,
}

/// A fresh [`Scenario`].
pub fn scenario() -> Scenario {
    scenario_with(&[])
}

/// A fresh [`Scenario`] whose configuration directory holds `files` before
/// the daemon starts: `(path under <XDG_CONFIG_HOME>/beachcomber, body)`, a
/// `conf.d/` file over the shipped ones, say.
pub fn scenario_with(files: &[(&str, &str)]) -> Scenario {
    let socket = IsolatedSocket::new();
    let home = socket.dir.path().join("home");
    let xdg = socket.dir.path().join("xdg");
    for (rel, body) in files {
        let path = xdg.join("beachcomber").join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).expect("make the config directory");
        std::fs::write(path, body).expect("write a config file");
    }
    let project = home.join("project");
    std::fs::create_dir_all(&project).expect("make the project directory");
    let project = project.canonicalize().expect("the project directory");
    let daemon = TestDaemon::serve_in_process(socket, home, xdg);
    Scenario { daemon, project }
}

impl Scenario {
    /// `comb check config --origins <key> -f json` over this daemon's
    /// configuration directory: one object per leaf, each naming the file
    /// that set it.
    pub fn origins(&self, key: &str) -> Vec<Value> {
        let out = self
            .daemon
            .command(&self.project)
            .args(["check", "--connect-timeout", "0", "config", "--origins"])
            .args([key, "-f", "json"])
            .output()
            .expect("run comb check config --origins");
        assert!(out.status.success(), "exit {:?}", out.status.code());
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).expect("one JSON object per line"))
            .collect()
    }

    /// `comb get -f json <key>` run in `dir`, with no environment but the
    /// daemon's isolation (`HOME`, `XDG_*`, the socket, `PATH`): the value,
    /// or `None` on a miss or a failure.
    pub fn get(&self, key: &str, dir: &Path) -> Option<Value> {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_comb"));
        cmd.env_clear();
        crate::common::daemon::isolate(
            &mut cmd,
            &self.daemon.home,
            &self.daemon.xdg,
            &self.daemon.socket.path,
        );
        let out = cmd
            .current_dir(dir)
            .args(["get", "-f", "json", key])
            .output()
            .expect("run comb get");
        out.status
            .success()
            .then(|| {
                serde_json::from_slice::<Value>(&out.stdout)
                    .unwrap_or_else(|e| panic!("get {key}: not JSON ({e}): {:?}", out.stdout))
            })
            .filter(|v| !v.is_null())
    }
}

/// A shipped file's template over the daemon's record, read in a project:
/// `terraform.workspace`, from `defaults/terraform.toml`, is the workspace
/// selected on disk when the shell sets none.
#[test]
fn a_scenario_reads_through_the_shipped_file() {
    let s = scenario();
    std::fs::create_dir_all(s.project.join(".terraform")).unwrap();
    std::fs::write(s.project.join(".terraform/environment"), "staging\n").unwrap();
    assert_eq!(
        s.get("terraform.workspace", &s.project),
        Some(json!("staging"))
    );
}
