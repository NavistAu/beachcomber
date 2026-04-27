// Option A: copied from beachcomber-client/tests/common/socket.rs.
// Both crates need an IsolatedSocket in their integration-test scope; duplicating the
// 12-line struct is simpler than bridging crate test boundaries with #[path = "..."].
use std::path::PathBuf;
use tempfile::TempDir;

pub struct IsolatedSocket {
    _dir: TempDir,
    pub path: PathBuf,
}

impl IsolatedSocket {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("beachcomber.sock");
        Self { _dir: dir, path }
    }
}
