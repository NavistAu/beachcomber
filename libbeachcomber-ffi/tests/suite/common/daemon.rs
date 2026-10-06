//! Same `DaemonGuard` spawn pattern as `libbeachcomber/tests/suite/common/daemon.rs`
//! (in-process daemon on an isolated socket, joined and shut down on Drop).
//! Not literally shared via `#[path]`: this crate's own `[lib] name` is
//! `beachcomber` too, so the root daemon crate is pulled in here under the
//! `comb_daemon` key (see Cargo.toml) to avoid an extern-crate-name
//! collision, and the import lines below reflect that.

use crate::common::socket::IsolatedSocket;
use comb_daemon::config::Config;
use comb_daemon::daemon;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use tokio::runtime::{Handle, Runtime};

/// RAII guard for an in-process daemon spawned in its own thread.
pub struct DaemonGuard {
    /// Socket path the daemon is listening on.
    pub path: PathBuf,
    /// Kept alive so the temp directory (and therefore the socket) survives.
    _socket: IsolatedSocket,
    /// Owned runtime — `Drop` takes it out via `Option::take` to call
    /// `shutdown_background(self)`, which requires ownership.
    runtime: Option<Runtime>,
    /// Thread handle so we can join after shutdown.
    thread: Option<thread::JoinHandle<()>>,
}

impl DaemonGuard {
    /// Spawn a new in-process daemon on an isolated socket path.
    /// Blocks until the daemon is ready (socket appears on disk).
    ///
    /// The daemon is handed a `Config` built in memory, which corresponds to
    /// no file: it seeds nothing, watches nothing, and never touches the
    /// developer's `~/.config/beachcomber`.
    pub fn spawn() -> Self {
        Self::start(Config::default(), false)
    }

    /// Spawn a daemon whose configuration directory is `config_dir`: it seeds
    /// `config_dir/defaults/*.toml`, `config.toml` and `conf.d/*.toml` the way
    /// `comb daemon` seeds `~/.config/beachcomber`, and reads nothing outside
    /// that directory. The config self-watch takes the polling seam so no
    /// kernel watch is registered on a directory the test deletes on exit.
    #[allow(dead_code)]
    pub fn spawn_with_config_dir(config_dir: &Path) -> Self {
        let config = Config {
            config_path: Some(config_dir.join("config.toml")),
            ..Config::default()
        };
        Self::start(config, true)
    }

    fn start(config: Config, polling: bool) -> Self {
        let iso = IsolatedSocket::new();
        let sock = iso.path.clone();

        let rt = Runtime::new().expect("build tokio runtime");
        let handle: Handle = rt.handle().clone();
        let sock_clone = sock.clone();

        let thread = thread::spawn(move || {
            handle.block_on(async {
                let (join, _ready) = if polling {
                    daemon::start_in_process_polling(sock_clone, config)
                } else {
                    daemon::start_in_process(sock_clone, config)
                };
                let _ = join.await;
            });
        });

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if sock.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }

        Self {
            path: sock,
            _socket: iso,
            runtime: Some(rt),
            thread: Some(thread),
        }
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_background();
        }
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}
