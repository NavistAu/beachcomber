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
