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
//!
//! [`served`] is the same shipped registry behind a server with no scheduler,
//! whose Sources see only the variables a test gives them: nothing watches
//! and nothing polls, so a read that answers a changed file found the change
//! itself (the record's stat inputs), and a test of what a file's lines make
//! of a file reads the daemon's half straight off the socket.

use crate::common::daemon::TestDaemon;
use crate::common::socket::IsolatedSocket;
use beachcomber::cache::Cache;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::server::Server;
use beachcomber::watcher_registry::WatcherRegistry;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod asdf;
mod battery;
mod direnv;
mod hostname;
mod kubecontext;
mod load;
mod mise;
mod network;
mod python;
mod terraform;
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
        self.get_in_env(key, dir, &[])
    }

    /// [`Scenario::get`] from a shell that also sets `vars`: what a
    /// template reads of the caller's environment.
    pub fn get_in_env(&self, key: &str, dir: &Path, vars: &[(&str, &str)]) -> Option<Value> {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_comb"));
        cmd.env_clear();
        crate::common::daemon::isolate(
            &mut cmd,
            &self.daemon.home,
            &self.daemon.xdg,
            &self.daemon.socket.path,
        );
        cmd.envs(vars.iter().copied());
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

/// The shipped defaults, with `conf` over them, served with no scheduler.
pub struct Served {
    /// The Sources' `HOME`, and this process's: a temp directory, canonical.
    pub home: PathBuf,
    /// An empty directory under `home`.
    pub project: PathBuf,
    sock: PathBuf,
    cache: Arc<Cache>,
    runtime: tokio::runtime::Runtime,
    _dir: tempfile::TempDir,
}

/// A [`Served`] whose Sources' lines see `HOME` (a fresh temp directory, also
/// this process's `HOME`, so its instance is the one at `~`) and `vars`, and
/// nothing else of this process's environment.
pub fn served(vars: &[(&str, &str)], conf: &str) -> Served {
    let dir = tempfile::tempdir().expect("a temp directory");
    let home = dir.path().canonicalize().unwrap().join("home");
    let project = home.join("project");
    std::fs::create_dir_all(&project).expect("make the project directory");
    // SAFETY: nextest gives each test its own process, and nothing else reads
    // the environment until the server below starts.
    unsafe { std::env::set_var("HOME", &home) };
    let mut env: std::collections::HashMap<String, String> = vars
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    env.insert("HOME".into(), home.to_string_lossy().into_owned());
    let layer: toml::Value = toml::from_str(conf).expect("the test's layer parses");
    let config =
        crate::common::registry::config_over_shipped(vec![(PathBuf::from("test.toml"), layer)]);
    let load = ProviderRegistry::load(&config, &env);
    assert!(load.errors.is_empty(), "{:?}", load.errors);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime");
    let watchers = Arc::new(WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let sock = dir.path().join("s");
    {
        let _enter = runtime.enter();
        let registry = load.registry.share(&cache);
        Server::new(sock.clone(), cache.clone(), registry, None, watchers)
            .spawn()
            .expect("serve");
    }
    Served {
        home,
        project,
        sock,
        cache,
        runtime,
        _dir: dir,
    }
}

impl Served {
    /// The daemon's answer to `get key` at `dir` (`None`, the global entry),
    /// waiting for any run the read makes: its `data`, or `None` for nothing
    /// there. A refusal fails the test.
    pub fn get(&self, key: &str, dir: Option<&Path>) -> Option<Value> {
        let mut request = json!({"op": "get", "key": key, "wait": "forever"});
        if let Some(dir) = dir {
            request["path"] = json!(dir);
        }
        let answer = self.ask(&request);
        assert_eq!(answer["ok"], json!(true), "get {key}: {answer}");
        answer.get("data").filter(|d| !d.is_null()).cloned()
    }

    /// One request, one answer.
    pub fn ask(&self, request: &Value) -> Value {
        let mut stream = std::os::unix::net::UnixStream::connect(&self.sock).expect("connect");
        writeln!(stream, "{request}").expect("send");
        let mut answer = String::new();
        BufReader::new(stream)
            .read_line(&mut answer)
            .expect("an answer");
        serde_json::from_str(&answer).expect("a JSON answer")
    }

    /// When `provider`'s Source `source` at the instance at `root` (`None`,
    /// the one at `~`) last ran: unchanged across reads that ran nothing.
    pub fn ran_at(
        &self,
        provider: &str,
        root: Option<&Path>,
        source: &str,
    ) -> tokio::time::Instant {
        let root = root.map(|r| r.to_string_lossy().into_owned());
        let _enter = self.runtime.enter();
        self.cache
            .get_source(provider, root.as_deref(), source)
            .unwrap_or_else(|| panic!("{provider}.{source}: no record at {root:?}"))
            .last_refreshed
    }
}

/// What the shipped registry holds of `provider`'s Source `source`: its
/// schedule (what it watches and polls, its failback, whether it is
/// nearest), and whether it has a root.
pub fn schedule(provider: &str, source: &str) -> (beachcomber::provider::SourceMetadata, bool) {
    let registry = crate::common::registry::with_shipped_defaults();
    let meta = registry
        .provider_sources(provider)
        .and_then(|sources| sources.iter().find(|s| s.name == source))
        .unwrap_or_else(|| panic!("{provider}.{source} is shipped"))
        .clone();
    let rooted = registry.source(provider, source).unwrap().has_root();
    (meta, rooted)
}

/// Give `path`, a file or a directory, an mtime well in the past, so a stamp
/// of it is not racy (a directory an entry was made in a moment ago is) and a
/// rewrite within the same second is a change.
pub fn age(path: &Path) {
    std::fs::File::open(path)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(10))
        .unwrap();
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
