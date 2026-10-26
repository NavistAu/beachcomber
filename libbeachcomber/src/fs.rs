//! The one reader of a file's `lstat`, shared by the `stat` template filter
//! and anything else that must say what a file looked like: size, mtime, kind
//! and permission bits.

use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// What a file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    Other,
}

impl Kind {
    /// The name `stat` shows: `file`, `dir`, `symlink` or `other`.
    pub fn name(self) -> &'static str {
        match self {
            Kind::File => "file",
            Kind::Dir => "dir",
            Kind::Symlink => "symlink",
            Kind::Other => "other",
        }
    }
}

/// A file as `lstat(2)` saw it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stat {
    pub size: u64,
    /// Modification time, whole seconds since the epoch.
    pub mtime_secs: i64,
    /// The nanoseconds after `mtime_secs`: zero where the filesystem keeps
    /// whole seconds.
    pub mtime_nanos: u32,
    pub kind: Kind,
    /// The permission bits, `st_mode & 0o7777`.
    pub mode: u32,
}

impl Stat {
    /// The fields of metadata already read, whether by `stat` or `lstat`.
    pub fn from_metadata(meta: &std::fs::Metadata) -> Stat {
        let ft = meta.file_type();
        let kind = if ft.is_symlink() {
            Kind::Symlink
        } else if ft.is_dir() {
            Kind::Dir
        } else if ft.is_file() {
            Kind::File
        } else {
            // A socket, fifo or device.
            Kind::Other
        };
        Stat {
            size: meta.len(),
            mtime_secs: meta.mtime(),
            mtime_nanos: u32::try_from(meta.mtime_nsec())
                .unwrap_or(0)
                .min(999_999_999),
            kind,
            mode: meta.mode() & 0o7777,
        }
    }

    /// The modification time as epoch seconds with its fraction.
    pub fn mtime(&self) -> f64 {
        self.mtime_secs as f64 + f64::from(self.mtime_nanos) / 1e9
    }
}

/// `lstat(2)` of `path`: a final symlink is described, not followed.
pub fn lstat(path: &Path) -> std::io::Result<Stat> {
    std::fs::symlink_metadata(path).map(|m| Stat::from_metadata(&m))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn lstat_reads_size_kind_mode_and_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "hello").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        let s = lstat(&file).unwrap();
        assert_eq!((s.size, s.kind, s.mode), (5, Kind::File, 0o640));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        assert!((now - s.mtime()).abs() < 120.0, "{} vs {now}", s.mtime());
        assert_eq!(lstat(dir.path()).unwrap().kind, Kind::Dir);
    }

    #[test]
    fn lstat_does_not_follow_a_final_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("t");
        std::fs::write(&target, "x").unwrap();
        let link = dir.path().join("l");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(lstat(&link).unwrap().kind, Kind::Symlink);
        assert_eq!(lstat(&target).unwrap().kind, Kind::File);
    }

    #[test]
    fn a_fraction_survives_in_mtime() {
        let s = Stat {
            size: 0,
            mtime_secs: 1_700_000_000,
            mtime_nanos: 500_000_000,
            kind: Kind::File,
            mode: 0,
        };
        assert_eq!(s.mtime(), 1_700_000_000.5);
    }

    #[test]
    fn a_missing_path_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let e = lstat(&dir.path().join("nope")).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
    }
}
