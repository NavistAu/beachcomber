use crate::common::socket::IsolatedSocket;
use beachcomber::config::Config;
use beachcomber::daemon;
use std::path::PathBuf;
use tokio::runtime::Runtime;

/// What the daemon composes when it starts: the managed `00-comb.toml` as a
/// layer, which carries the abstract provider's lines. An in-process daemon
/// has no `defaults/` directory to read it from.
fn shipped_defaults() -> Config {
    Config::from_layers(daemon::defaults::shipped_layers())
}

/// RAII guard for an in-process daemon on its own two-worker runtime, which
/// runs it while the test thread makes blocking client calls.
///
/// On `Drop`, the runtime is shut down in the background, which aborts the
/// daemon's tasks. The `Runtime` is owned directly (not Arc-wrapped) so that
/// `Drop` can call `shutdown_background(self)`, which takes ownership.
pub struct DaemonGuard {
    /// Socket path the daemon is listening on.
    pub path: PathBuf,
    /// Kept alive so the temp directory (and therefore the socket) survives.
    _socket: IsolatedSocket,
    runtime: Option<Runtime>,
}

impl DaemonGuard {
    /// Spawn a new in-process daemon on an isolated socket path, and return
    /// once it serves it: the daemon's own `DaemonReady`, not a poll of the
    /// socket file. Its file watching polls, as no test here is about the
    /// kernel's watch backend.
    pub fn spawn() -> Self {
        let iso = IsolatedSocket::new();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("build tokio runtime");
        let (_task, ready) = {
            let _enter = runtime.enter();
            daemon::start_in_process_polling(iso.path.clone(), shipped_defaults())
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
        // Take ownership of the runtime so we can call shutdown_background(self),
        // which consumes the Runtime — impossible through Arc or &mut.
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_background();
        }
    }
}
