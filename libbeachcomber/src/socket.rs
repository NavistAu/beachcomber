//! Where the daemon's socket is: one resolution for the CLI, the daemon and
//! every SDK.
//!
//! In order: the CLI's `--socket` flag, a configured `[daemon] socket_path`
//! (the CLI and daemon read the config file; the SDKs pass none), then
//! `$BEACHCOMBER_SOCKET`, then the stable per-user default
//! `/tmp/beachcomber-<uid>/sock`. No session-scoped environment (`$TMPDIR`,
//! `$XDG_RUNTIME_DIR`) is consulted: a daemon is one per user, not one per
//! session.

use std::path::{Path, PathBuf};

/// The longest unix socket path a daemon can bind, exclusive: `sun_path` is
/// 104 bytes on macOS and 108 on Linux, and the conservative bound is used on
/// both so the rule is the same everywhere.
pub const MAX_SOCKET_PATH_BYTES: usize = 104;

/// Which step of [`resolve`] chose the socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SocketSource {
    /// The CLI's `--socket`.
    Flag,
    /// `[daemon] socket_path` in the configuration.
    Config,
    /// `$BEACHCOMBER_SOCKET`.
    Env,
    /// `/tmp/beachcomber-<uid>/sock`.
    Default,
}

impl SocketSource {
    /// Whether a daemon started on this socket is spared by the reaping
    /// daemon: one this process chose for itself (`--socket`,
    /// `$BEACHCOMBER_SOCKET`) is deliberate, not an orphan of the per-user
    /// one.
    pub fn no_reap(self) -> bool {
        matches!(self, SocketSource::Flag | SocketSource::Env)
    }
}

/// The socket the caller means, and which step chose it. `flag` is the CLI's
/// `--socket`; `configured` is `[daemon] socket_path`, `~` already expanded.
pub fn resolve(flag: Option<&Path>, configured: Option<&Path>) -> (PathBuf, SocketSource) {
    if let Some(p) = flag {
        return (p.to_path_buf(), SocketSource::Flag);
    }
    if let Some(p) = configured {
        return (p.to_path_buf(), SocketSource::Config);
    }
    if let Some(env) = std::env::var_os("BEACHCOMBER_SOCKET")
        && !env.is_empty()
    {
        return (PathBuf::from(env), SocketSource::Env);
    }
    (default_path(), SocketSource::Default)
}

/// `/tmp/beachcomber-<uid>/sock`.
pub fn default_path() -> PathBuf {
    let uid = unsafe { libc::getuid() };
    PathBuf::from("/tmp")
        .join(format!("beachcomber-{uid}"))
        .join("sock")
}

/// A socket path the kernel cannot bind, refused before anything is spawned
/// to fail on it.
pub fn check_length(socket: &Path) -> Result<(), String> {
    let len = socket.as_os_str().len();
    if len >= MAX_SOCKET_PATH_BYTES {
        return Err(format!(
            "socket path is {len} bytes; unix sockets are limited to {MAX_SOCKET_PATH_BYTES} \
             (SUN_LEN): {}",
            socket.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flag_beats_the_config_and_the_config_beats_the_rest() {
        let (p, s) = resolve(Some(Path::new("/f")), Some(Path::new("/c")));
        assert_eq!((p, s), (PathBuf::from("/f"), SocketSource::Flag));
        let (p, s) = resolve(None, Some(Path::new("/c")));
        assert_eq!((p, s), (PathBuf::from("/c"), SocketSource::Config));
    }

    #[test]
    fn beachcomber_socket_beats_the_default_and_an_empty_one_is_unset() {
        // SAFETY: nextest runs every test in its own process.
        unsafe { std::env::set_var("BEACHCOMBER_SOCKET", "/env/sock") };
        assert_eq!(
            resolve(None, None),
            (PathBuf::from("/env/sock"), SocketSource::Env)
        );
        unsafe { std::env::set_var("BEACHCOMBER_SOCKET", "") };
        assert_eq!(resolve(None, None), (default_path(), SocketSource::Default));
    }

    #[test]
    fn the_default_is_per_user_under_tmp() {
        let p = default_path();
        let uid = unsafe { libc::getuid() };
        assert_eq!(p, PathBuf::from(format!("/tmp/beachcomber-{uid}/sock")));
    }

    #[test]
    fn only_a_socket_this_process_chose_is_spared_reaping() {
        assert!(SocketSource::Flag.no_reap());
        assert!(SocketSource::Env.no_reap());
        assert!(!SocketSource::Config.no_reap());
        assert!(!SocketSource::Default.no_reap());
    }

    #[test]
    fn a_path_too_long_to_bind_is_refused_naming_it() {
        let long = format!("/{}", "x".repeat(MAX_SOCKET_PATH_BYTES));
        let err = check_length(Path::new(&long)).unwrap_err();
        assert!(err.contains("SUN_LEN") && err.contains(&long), "{err}");
        assert!(check_length(Path::new("/tmp/s")).is_ok());
    }
}
