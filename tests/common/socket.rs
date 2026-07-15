// Option A: copied from beachcomber-client/tests/common/socket.rs.
// Both crates need an IsolatedSocket in their integration-test scope; duplicating the
// 12-line struct is simpler than bridging crate test boundaries with #[path = "..."].
use std::path::PathBuf;
use tempfile::TempDir;

#[allow(dead_code)]
pub struct IsolatedSocket {
    pub dir: TempDir,
    pub path: PathBuf,
}

impl IsolatedSocket {
    /// Create an isolated socket at `<tempdir>/beachcomber/sock`.
    ///
    /// Tests point a process at exactly this socket via `BEACHCOMBER_SOCKET`
    /// (or `--socket`); canonical resolution consults no session-scoped
    /// environment, so a tempdir path can only reach a process explicitly.
    // Used by multiple integration test binaries via the shared common module.
    #[allow(dead_code)]
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        // The daemon's server creates parent dirs via `create_dir_all`, so we
        // don't need to pre-create the `beachcomber/` subdirectory here.
        let path = dir.path().join("beachcomber").join("sock");
        Self { dir, path }
    }
}
