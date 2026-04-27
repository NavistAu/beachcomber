//! DaemonSpawner boundary trait — abstracts daemon fork and socket readiness polling.

#[cfg_attr(test, mockall::automock)]
pub trait DaemonSpawner: Send + Sync {
    /// Fork the daemon process at `binary_path`, directing it to listen on `socket_path`.
    /// Returns `Ok(())` on successful spawn; the child runs in the background.
    fn fork_daemon(&self, binary_path: &str, socket_path: &std::path::Path) -> std::io::Result<()>;

    /// Poll `socket_path` until the daemon accepts connections or `attempts` are exhausted.
    /// Returns `true` if the socket became reachable within the attempt budget.
    fn wait_for_socket(&self, socket_path: &std::path::Path, attempts: u32) -> bool;
}

pub struct RealDaemonSpawner;

impl DaemonSpawner for RealDaemonSpawner {
    fn fork_daemon(&self, binary_path: &str, socket_path: &std::path::Path) -> std::io::Result<()> {
        crate::daemon::fork_daemon(binary_path, socket_path)
    }

    fn wait_for_socket(&self, socket_path: &std::path::Path, attempts: u32) -> bool {
        crate::daemon::wait_for_daemon(socket_path, attempts)
    }
}
