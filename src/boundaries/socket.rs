//! SocketProbe boundary trait — abstracts "is a daemon serving this socket?" from OS state.
//!
//! Where the socket is, is `libbeachcomber::socket::resolve`'s alone.

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
