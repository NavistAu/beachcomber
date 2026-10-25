//! Same `DaemonGuard` as `libbeachcomber/tests/suite/common/daemon.rs`
//! (in-process daemon on an isolated socket, shut down on Drop). Not
//! literally shared via `#[path]`: this crate's own `[lib] name` is
//! `beachcomber` too, so the root daemon crate is pulled in here under the
//! `comb_daemon` key (see Cargo.toml) to avoid an extern-crate-name
//! collision, and the import lines below reflect that.

use crate::common::socket::IsolatedSocket;
use comb_daemon::config::Config;
use comb_daemon::daemon;
use std::path::{Path, PathBuf};
use tokio::runtime::Runtime;

/// What the daemon composes when it starts: the managed `00-comb.toml` as a
/// layer, which carries the abstract provider's lines. An in-process daemon
/// handed no directory has no `defaults/` to read it from.
fn shipped_defaults() -> Config {
    Config::from_layers(vec![daemon::defaults::comb_layer()])
}

/// RAII guard for an in-process daemon on its own two-worker runtime, which
/// runs it while the test thread makes blocking FFI calls.
pub struct DaemonGuard {
    /// Socket path the daemon is listening on.
    pub path: PathBuf,
    /// Kept alive so the temp directory (and therefore the socket) survives.
    _socket: IsolatedSocket,
    /// Owned runtime — `Drop` takes it out via `Option::take` to call
    /// `shutdown_background(self)`, which requires ownership.
    runtime: Option<Runtime>,
}

impl DaemonGuard {
    /// Spawn a new in-process daemon on an isolated socket path, and return
    /// once it serves it (its own `DaemonReady`, not a poll of the socket
    /// file).
    ///
    /// The daemon is handed a `Config` built in memory, which corresponds to
    /// no file: it seeds nothing, watches nothing, and never touches the
    /// developer's `~/.config/beachcomber`.
    pub fn spawn() -> Self {
        Self::start(shipped_defaults())
    }

    /// Spawn a daemon whose configuration directory is `config_dir`: it seeds
    /// `config_dir/defaults/*.toml`, `config.toml` and `conf.d/*.toml` the way
    /// `comb daemon` seeds `~/.config/beachcomber`, and reads nothing outside
    /// that directory.
    #[allow(dead_code)]
    pub fn spawn_with_config_dir(config_dir: &Path) -> Self {
        let config = Config::load_from(&config_dir.join("config.toml"));
        Self::start(config)
    }

    /// Every daemon here polls for file changes (the polling seam): no test
    /// is about the kernel's watch backend, and none registers a kernel
    /// watch on a directory the test deletes on exit.
    fn start(config: Config) -> Self {
        let iso = IsolatedSocket::new();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("build tokio runtime");
        let (_task, ready) = {
            let _enter = runtime.enter();
            daemon::start_in_process_polling(iso.path.clone(), config)
        };
        runtime
            .block_on(ready)
            .expect("the in-process daemon refused to start");
        Self {
            path: iso.path.clone(),
            _socket: iso,
            runtime: Some(runtime),
        }
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_background();
        }
    }
}
