//! SocketDiscovery boundary trait — abstracts socket-path resolution from OS state.

#[cfg_attr(test, mockall::automock)]
pub trait SocketDiscovery: Send + Sync {
    fn default_socket_path(&self) -> std::path::PathBuf;
    fn xdg_runtime_dir(&self) -> Option<std::path::PathBuf>;
    fn tmpdir(&self) -> std::path::PathBuf;
}

pub struct RealSocketDiscovery;

impl SocketDiscovery for RealSocketDiscovery {
    /// Mirror the algorithm in `Config::resolve_socket_path` for the no-config-override case.
    ///
    /// Priority:
    /// 1. `$XDG_RUNTIME_DIR/beachcomber/sock`
    /// 2. `/tmp/beachcomber-<uid>/sock`
    fn default_socket_path(&self) -> std::path::PathBuf {
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            return std::path::PathBuf::from(runtime_dir)
                .join("beachcomber")
                .join("sock");
        }

        let uid = unsafe { libc::getuid() };
        std::path::PathBuf::from("/tmp")
            .join(format!("beachcomber-{uid}"))
            .join("sock")
    }

    fn xdg_runtime_dir(&self) -> Option<std::path::PathBuf> {
        std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from)
    }

    fn tmpdir(&self) -> std::path::PathBuf {
        std::env::temp_dir()
    }
}
