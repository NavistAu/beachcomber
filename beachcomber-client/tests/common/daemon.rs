use crate::common::socket::IsolatedSocket;
use beachcomber::config::Config;
use beachcomber::daemon;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tokio::runtime::Runtime;

/// RAII guard for an in-process daemon spawned in its own thread.
///
/// On `Drop`, the underlying tokio `Runtime` is shut down (background shutdown,
/// which aborts all still-running tasks), and the thread is joined.
/// This replaces detached `thread::spawn` calls that previously leaked daemon
/// threads across test boundaries.
pub struct DaemonGuard {
    /// Socket path the daemon is listening on.
    pub path: PathBuf,
    /// Kept alive so the temp directory (and therefore the socket) survives.
    _socket: IsolatedSocket,
    /// Shared runtime — calling `shutdown_background` stops the daemon tasks.
    runtime: Arc<Runtime>,
    /// Thread handle so we can join after shutdown.
    thread: Option<thread::JoinHandle<()>>,
}

impl DaemonGuard {
    /// Spawn a new in-process daemon on an isolated socket path.
    /// Blocks until the daemon is ready (socket appears on disk).
    pub fn spawn() -> Self {
        let iso = IsolatedSocket::new();
        let sock = iso.path.clone();

        let rt = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("build tokio runtime"),
        );
        let rt_clone = Arc::clone(&rt);
        let sock_clone = sock.clone();

        let handle = thread::spawn(move || {
            rt_clone.block_on(async {
                let join = daemon::start_in_process(sock_clone, Config::default());
                // Drive the daemon until the runtime is shut down externally.
                let _ = join.await;
            });
        });

        // Wait for the socket file to appear (up to 2s).
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
            runtime: rt,
            thread: Some(handle),
        }
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        // Shut down the runtime — aborts all tasks including the daemon.
        self.runtime.shutdown_background();
        // Join the thread so we don't leave zombie OS threads.
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}
