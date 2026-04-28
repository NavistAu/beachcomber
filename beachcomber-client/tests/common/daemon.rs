use crate::common::socket::IsolatedSocket;
use beachcomber::config::Config;
use beachcomber::daemon;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use tokio::runtime::{Handle, Runtime};

/// RAII guard for an in-process daemon spawned in its own thread.
///
/// On `Drop`, the underlying tokio `Runtime` is shut down (background shutdown,
/// which aborts all still-running tasks), and the thread is joined.
/// This replaces detached `thread::spawn` calls that previously leaked daemon
/// threads across test boundaries.
///
/// The `Runtime` is owned directly (not Arc-wrapped) so that `Drop` can call
/// `shutdown_background(self)`, which takes ownership. The worker thread
/// receives a `Handle` (Clone + Send + Sync + 'static) instead.
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
    pub fn spawn() -> Self {
        let iso = IsolatedSocket::new();
        let sock = iso.path.clone();

        let rt = Runtime::new().expect("build tokio runtime");
        // Pass a Handle into the worker thread — Handle is Clone + Send + Sync + 'static,
        // so it crosses the thread boundary without needing to share the Runtime itself.
        let handle: Handle = rt.handle().clone();
        let sock_clone = sock.clone();

        let thread = thread::spawn(move || {
            handle.block_on(async {
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
            runtime: Some(rt),
            thread: Some(thread),
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
        // Join the thread so we don't leave zombie OS threads.
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}
