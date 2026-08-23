//! SocketDiscovery boundary trait — abstracts socket-path resolution from OS state.
//! SocketProbe boundary trait — abstracts "is a daemon serving this socket?" from OS state.

#[cfg_attr(test, mockall::automock)]
pub trait SocketDiscovery: Send + Sync {
    fn default_socket_path(&self) -> std::path::PathBuf;
    fn xdg_runtime_dir(&self) -> Option<std::path::PathBuf>;
    fn tmpdir(&self) -> std::path::PathBuf;
}

/// Whether a daemon is actively serving a given Unix socket.
///
/// The real implementation attempts a `connect()`; tests inject a double that
/// answers without touching the OS. Used by the singleton's same-build serving
/// probe (see `docs/canon/singleton.md` §"Same-build serving probe").
#[cfg_attr(test, mockall::automock)]
pub trait SocketProbe: Send + Sync {
    /// Returns `true` if a process is accepting connections on `socket`.
    fn is_serving(&self, socket: &std::path::Path) -> bool;
}

pub struct RealSocketProbe;

impl SocketProbe for RealSocketProbe {
    fn is_serving(&self, socket: &std::path::Path) -> bool {
        std::os::unix::net::UnixStream::connect(socket).is_ok()
    }
}

pub struct RealSocketDiscovery;

impl SocketDiscovery for RealSocketDiscovery {
    /// Mirror the algorithm in `Config::resolve_socket_path` for the
    /// no-config-override, no-`$BEACHCOMBER_SOCKET` case: the stable per-user
    /// default `/tmp/beachcomber-<uid>/sock`. Consults no session-scoped
    /// environment (see docs/canon/singleton.md).
    fn default_socket_path(&self) -> std::path::PathBuf {
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
