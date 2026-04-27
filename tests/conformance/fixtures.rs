use std::path::PathBuf;
use tempfile::TempDir;

pub struct ConformanceFixture {
    _dir: TempDir,
    pub path: PathBuf,
}

impl ConformanceFixture {
    /// A directory that exists, is empty, and is writable.
    pub fn empty() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_path_buf();
        Self { _dir: dir, path }
    }

    /// A directory that does not exist on disk.
    pub fn missing() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("does-not-exist");
        Self { _dir: dir, path }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_fixture_path_exists() {
        let f = ConformanceFixture::empty();
        assert!(f.path.exists());
    }

    #[test]
    fn missing_fixture_path_does_not_exist() {
        let f = ConformanceFixture::missing();
        assert!(!f.path.exists());
    }
}
