//! The one reader of a file's `lstat`, shared by the `stat` template filter
//! and anything else that must say what a file looked like: size, mtime, kind
//! and permission bits. Beside it, a path's [`Stamp`] for the validity hash
//! ([`crate::validity`]), what a render read stamped before it read it
//! ([`Touched`]), and what a filesystem's stamps can be trusted for
//! ([`mount_of`]).

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

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

/// What a path looked like, for the validity hash: one input of a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stamp {
    /// Nothing is there: its creation is a change.
    Absent,
    /// It could not be stamped (`EACCES`, say): nothing is known of it, and a
    /// record that reads it is kept.
    Unknown,
    Present(Present),
}

/// A path that is there, as `stat(2)` (following symlinks) saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Present {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_secs: i64,
    pub mtime_nanos: u32,
    /// What it is once followed: never [`Kind::Symlink`].
    pub kind: Kind,
    /// The link's own inode and mtime when the path is a symlink, so that
    /// pointing it elsewhere is a change.
    pub link: Option<Link>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Link {
    pub ino: u64,
    pub mtime_secs: i64,
    pub mtime_nanos: u32,
}

impl Kind {
    fn code(self) -> u8 {
        match self {
            Kind::File => 0,
            Kind::Dir => 1,
            Kind::Symlink => 2,
            Kind::Other => 3,
        }
    }
}

impl Stamp {
    /// The canonical encoding the validity hash takes: a tag (0 absent, 1
    /// unknown, 2 present); for a present path its `dev`, `ino`, `size` (u64),
    /// `mtime` seconds (i64) and nanoseconds (u32), kind (u8: file 0, dir 1,
    /// other 3) and whether it is a link (u8), all little-endian; for a link
    /// its own `ino` (u64) and `mtime` (i64, u32). Stored hashes depend on
    /// it: it does not change.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Stamp::Absent => vec![0],
            Stamp::Unknown => vec![1],
            Stamp::Present(p) => {
                let mut out = Vec::with_capacity(48);
                out.push(2);
                out.extend(p.dev.to_le_bytes());
                out.extend(p.ino.to_le_bytes());
                out.extend(p.size.to_le_bytes());
                out.extend(p.mtime_secs.to_le_bytes());
                out.extend(p.mtime_nanos.to_le_bytes());
                out.push(p.kind.code());
                match p.link {
                    None => out.push(0),
                    Some(link) => {
                        out.push(1);
                        out.extend(link.ino.to_le_bytes());
                        out.extend(link.mtime_secs.to_le_bytes());
                        out.extend(link.mtime_nanos.to_le_bytes());
                    }
                }
                out
            }
        }
    }
}

fn nanos(meta: &std::fs::Metadata) -> u32 {
    u32::try_from(meta.mtime_nsec())
        .unwrap_or(0)
        .min(999_999_999)
}

/// The stamp of `path`: `stat(2)` following symlinks, and `lstat(2)` of a
/// symlink for its own inode and mtime. Nothing there, or a component that
/// is not a directory, is [`Stamp::Absent`], and so is a symlink to nothing;
/// any other failure is [`Stamp::Unknown`]. A directory is stamped as a node,
/// by its own mtime, which an entry created, removed or renamed in it changes.
///
/// One `lstat(2)` for what is not a symlink, the common input: a read pays a
/// stamp per input.
pub fn stamp(path: &Path) -> Stamp {
    let absent = |e: &std::io::Error| {
        matches!(
            e.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
        )
    };
    let own = match std::fs::symlink_metadata(path) {
        Ok(own) => own,
        Err(e) if absent(&e) => return Stamp::Absent,
        Err(_) => return Stamp::Unknown,
    };
    let (followed, link) = if own.file_type().is_symlink() {
        let link = Link {
            ino: own.ino(),
            mtime_secs: own.mtime(),
            mtime_nanos: nanos(&own),
        };
        match std::fs::metadata(path) {
            Ok(followed) => (followed, Some(link)),
            Err(e) if absent(&e) => return Stamp::Absent,
            Err(_) => return Stamp::Unknown,
        }
    } else {
        (own, None)
    };
    let kind = match Stat::from_metadata(&followed).kind {
        Kind::Symlink => Kind::Other,
        kind => kind,
    };
    Stamp::Present(Present {
        dev: followed.dev(),
        ino: followed.ino(),
        size: followed.len(),
        mtime_secs: followed.mtime(),
        mtime_nanos: nanos(&followed),
        kind,
        link,
    })
}

/// How much of a path its reader's answer depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Regard {
    /// What is in it: any stamp field moving is a change.
    Whole,
    /// That it is there, and what kind of thing it is, on which device (so a
    /// mount's trust still applies). A directory's entries coming and going
    /// are no change.
    Presence,
}

impl Regard {
    /// The stamp of `path`, as far as this regard goes.
    pub fn stamp(self, path: &Path) -> Stamp {
        match (self, stamp(path)) {
            (Regard::Presence, Stamp::Present(p)) => Stamp::Present(Present {
                dev: p.dev,
                ino: 0,
                size: 0,
                mtime_secs: 0,
                mtime_nanos: 0,
                kind: p.kind,
                link: None,
            }),
            (_, stamp) => stamp,
        }
    }
}

/// A path a render read, stamped immediately before it was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Touch {
    pub path: PathBuf,
    pub stamp: Stamp,
    /// How much of it the stamp covers: a later check stamps it the same way.
    pub regard: Regard,
    /// When the stamp was taken: read off the clock first, so it never trails
    /// the stamp. A stamp's racy window is measured from it.
    pub at: SystemTime,
}

impl Touch {
    /// Stamp `path` now.
    pub fn take(path: &Path) -> Touch {
        Self::regarding(path, Regard::Whole)
    }

    /// Stamp `path` now, for what its presence says ([`Regard::Presence`]).
    pub fn presence(path: &Path) -> Touch {
        Self::regarding(path, Regard::Presence)
    }

    fn regarding(path: &Path, regard: Regard) -> Touch {
        let at = SystemTime::now();
        Touch {
            stamp: regard.stamp(path),
            path: path.to_path_buf(),
            regard,
            at,
        }
    }
}

/// The most paths one render's reads are kept for: a render that reads more
/// keeps the first this many.
pub const MOST_TOUCHED: usize = 64;

/// What a render read, where someone keeps account of it (a daemon's run of a
/// Source, through [`crate::engine::EngineCtx::touched`]): each path once, in
/// the order it was first read, stamped before that read. Past
/// [`MOST_TOUCHED`] a path is counted, not stamped. A path read for its presence
/// and then for what is in it is both: each stamp checks what it was read for.
#[derive(Debug, Default)]
pub struct Touched {
    state: Mutex<TouchedState>,
}

#[derive(Debug, Default)]
struct TouchedState {
    touches: Vec<Touch>,
    seen: HashSet<(PathBuf, Regard)>,
}

impl Touched {
    fn state(&self) -> std::sync::MutexGuard<'_, TouchedState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `path` is about to be read: stamp it, unless it has been already.
    pub fn touch(&self, path: &Path) {
        let mut state = self.state();
        if !state.seen.insert((path.to_path_buf(), Regard::Whole)) {
            return;
        }
        if state.touches.len() < MOST_TOUCHED {
            state.touches.push(Touch::take(path));
        }
    }

    /// Paths read, each stamped by the caller before it read it: what a walk
    /// stamped ahead of probing, kept once it knows what it probed.
    pub fn keep(&self, touches: impl IntoIterator<Item = Touch>) {
        let mut state = self.state();
        for touch in touches {
            if !state.seen.insert((touch.path.clone(), touch.regard)) {
                continue;
            }
            if state.touches.len() < MOST_TOUCHED {
                state.touches.push(touch);
            }
        }
    }

    /// The paths read so far, stamped, in the order first read.
    pub fn touches(&self) -> Vec<Touch> {
        self.state().touches.clone()
    }

    /// How many paths have been read, those past [`MOST_TOUCHED`] included.
    pub fn count(&self) -> usize {
        self.state().seen.len()
    }
}

/// What a filesystem's stamps can be trusted for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mount {
    /// A local filesystem whose mtimes have this resolution: nanoseconds for
    /// APFS, ext4, btrfs, xfs, tmpfs and their like; a second for HFS+; two
    /// for FAT and exFAT.
    Local { resolution: Duration },
    /// A network or FUSE mount, or one not known: a stat there can hang, and
    /// its attribute cache can lie, so its stamps are not taken on a read.
    Remote { kind: Option<String> },
}

/// The [`Mount`] the device `dev`, which `path` lies on, is: classified once,
/// by `statfs(2)` of `path`, and kept for the process's life. `true` beside
/// it the first time a device is seen, so a caller can say once what it found.
pub fn mount_of(path: &Path, dev: u64) -> (Mount, bool) {
    static MOUNTS: OnceLock<Mutex<HashMap<u64, Mount>>> = OnceLock::new();
    let mounts = MOUNTS.get_or_init(Mutex::default);
    if let Some(known) = mounts.lock().unwrap_or_else(|e| e.into_inner()).get(&dev) {
        return (known.clone(), false);
    }
    let mount = classify(path);
    mounts
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(dev, mount.clone());
    (mount, true)
}

/// `statfs(2)` of `path`, as a [`Mount`].
fn classify(path: &Path) -> Mount {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return Mount::Remote { kind: None };
    };
    let mut fs = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: `c_path` is a NUL-terminated path and `fs` is a properly sized
    // out-parameter, read only when the call succeeds.
    if unsafe { libc::statfs(c_path.as_ptr(), fs.as_mut_ptr()) } != 0 {
        return Mount::Remote { kind: None };
    }
    // SAFETY: statfs succeeded and filled it in.
    let fs = unsafe { fs.assume_init() };
    kind_of(&fs)
}

const NANOS: Duration = Duration::from_nanos(1);

#[cfg(target_os = "macos")]
fn kind_of(fs: &libc::statfs) -> Mount {
    let name: String = fs
        .f_fstypename
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8 as char)
        .collect();
    named(name)
}

/// A mount by `statfs(2)`'s `f_fstypename`.
#[cfg(target_os = "macos")]
fn named(name: String) -> Mount {
    let resolution = match name.as_str() {
        "apfs" | "tmpfs" | "devfs" => NANOS,
        "hfs" => Duration::from_secs(1),
        "msdos" | "exfat" => Duration::from_secs(2),
        _ => return Mount::Remote { kind: Some(name) },
    };
    Mount::Local { resolution }
}

#[cfg(target_os = "linux")]
fn kind_of(fs: &libc::statfs) -> Mount {
    magic(fs.f_type as u64 & 0xffff_ffff)
}

/// A mount by `statfs(2)`'s `f_type`, the filesystem's magic number.
#[cfg(target_os = "linux")]
fn magic(magic: u64) -> Mount {
    let resolution = match magic {
        // ext2/3/4, btrfs, xfs, tmpfs, overlayfs, zfs, f2fs, bcachefs, ramfs.
        0xEF53 | 0x9123_683E | 0x5846_5342 | 0x0102_1994 | 0x794C_7630 | 0x2FC1_2FC1
        | 0xF2F5_2010 | 0xCA45_1A4E | 0x8584_58F6 => NANOS,
        // msdos/vfat, exfat.
        0x4D44 | 0x2011_BAB0 => Duration::from_secs(2),
        // HFS+.
        0x482B => Duration::from_secs(1),
        _ => {
            return Mount::Remote {
                kind: Some(format!("{magic:#x}")),
            };
        }
    };
    Mount::Local { resolution }
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

    fn present(path: &Path) -> Present {
        match stamp(path) {
            Stamp::Present(p) => p,
            other => panic!("{path:?}: {other:?}"),
        }
    }

    #[test]
    fn a_stamp_of_an_absent_path_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(stamp(&dir.path().join("nope")), Stamp::Absent);
        let file = dir.path().join("f");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(
            stamp(&file.join("below")),
            Stamp::Absent,
            "a component that is a file"
        );
    }

    #[test]
    fn a_stamp_of_a_file_and_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "hello").unwrap();
        let p = present(&file);
        assert_eq!((p.kind, p.size, p.link), (Kind::File, 5, None));
        let d = present(dir.path());
        assert_eq!(d.kind, Kind::Dir);
        assert_eq!(p.dev, d.dev);
    }

    #[test]
    fn a_stamp_of_a_symlink_carries_the_link_too() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::write(&a, "1").unwrap();
        std::fs::write(&b, "2").unwrap();
        let link = dir.path().join("l");
        std::os::unix::fs::symlink(&a, &link).unwrap();
        let first = present(&link);
        assert_eq!(first.kind, Kind::File, "followed");
        assert!(first.link.is_some());
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&b, &link).unwrap();
        assert_ne!(stamp(&link), Stamp::Present(first), "retargeted");
    }

    #[test]
    fn a_symlink_to_nothing_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("l");
        std::os::unix::fs::symlink(dir.path().join("nothing"), &link).unwrap();
        assert_eq!(stamp(&link), Stamp::Absent);
    }

    #[test]
    fn a_stamp_of_what_cannot_be_read_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("f"), "x").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let got = stamp(&locked.join("f"));
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        if std::fs::metadata(locked.join("f")).is_ok() && got != Stamp::Unknown {
            // Root reads through any mode.
            return;
        }
        assert_eq!(got, Stamp::Unknown);
    }

    /// A rename over a file is a new inode: the stamp changes even when the
    /// size and the mtime do not.
    #[test]
    fn a_same_size_rename_is_a_new_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let (f, tmp) = (dir.path().join("f"), dir.path().join("f.tmp"));
        std::fs::write(&f, "aaaa").unwrap();
        let mtime = std::fs::metadata(&f).unwrap().modified().unwrap();
        let before = present(&f);
        std::fs::write(&tmp, "bbbb").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&tmp)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        std::fs::rename(&tmp, &f).unwrap();
        let after = present(&f);
        assert_eq!(
            (after.size, after.mtime_secs, after.mtime_nanos),
            (before.size, before.mtime_secs, before.mtime_nanos)
        );
        assert_ne!(after.ino, before.ino);
        let hash = |p: Present| crate::validity::stamps_hash(&[(&f, Stamp::Present(p))]);
        assert_ne!(hash(after), hash(before), "the hash changes with it");
    }

    #[test]
    fn the_temp_directory_is_a_local_mount_classified_once() {
        let dir = tempfile::tempdir().unwrap();
        let dev = present(dir.path()).dev;
        let (mount, _) = mount_of(dir.path(), dev);
        assert!(matches!(mount, Mount::Local { .. }), "{mount:?}");
        let (again, first) = mount_of(dir.path(), dev);
        assert_eq!((again, first), (mount, false));
    }

    /// Network and FUSE mounts, and any not known, are not stat'd on a read.
    #[cfg(target_os = "macos")]
    #[test]
    fn statfs_classifies_network_and_fuse_mounts_as_remote() {
        for remote in [
            "nfs", "smbfs", "afpfs", "webdav", "macfuse", "osxfuse", "zzz",
        ] {
            assert_eq!(
                named(remote.into()),
                Mount::Remote {
                    kind: Some(remote.into())
                }
            );
        }
        let local = |resolution| Mount::Local { resolution };
        assert_eq!(named("apfs".into()), local(NANOS));
        assert_eq!(named("hfs".into()), local(Duration::from_secs(1)));
        assert_eq!(named("msdos".into()), local(Duration::from_secs(2)));
        assert_eq!(named("exfat".into()), local(Duration::from_secs(2)));
    }

    /// Network and FUSE mounts, and any not known, are not stat'd on a read.
    #[cfg(target_os = "linux")]
    #[test]
    fn statfs_classifies_network_and_fuse_mounts_as_remote() {
        // NFS, FUSE, SMB, CIFS, SMB2, and one not known.
        for remote in [
            0x6969,
            0x6573_5546,
            0x517B,
            0xFF53_4D42,
            0xFE53_4D42,
            0x1234,
        ] {
            assert!(matches!(magic(remote), Mount::Remote { .. }), "{remote:#x}");
        }
        let local = |resolution| Mount::Local { resolution };
        assert_eq!(magic(0xEF53), local(NANOS), "ext4");
        assert_eq!(magic(0x0102_1994), local(NANOS), "tmpfs");
        assert_eq!(magic(0x482B), local(Duration::from_secs(1)), "HFS+");
        assert_eq!(magic(0x4D44), local(Duration::from_secs(2)), "FAT");
    }

    /// Each path once, in the order first read, stamped when first read: a
    /// second read of it keeps the first stamp, which is the earlier.
    #[test]
    fn touched_keeps_each_path_once_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::write(&a, "1").unwrap();
        let touched = Touched::default();
        touched.touch(&a);
        touched.touch(&b);
        std::fs::write(&a, "22").unwrap();
        touched.touch(&a);
        touched.keep([Touch::take(&b), Touch::take(&dir.path().join("c"))]);
        let got = touched.touches();
        let paths: Vec<&Path> = got.iter().map(|t| t.path.as_path()).collect();
        assert_eq!(paths, [a.as_path(), b.as_path(), &dir.path().join("c")]);
        assert!(
            matches!(got[0].stamp, Stamp::Present(p) if p.size == 1),
            "{got:?}"
        );
        assert_eq!(got[1].stamp, Stamp::Absent);
        assert!(got[0].at <= got[1].at);
    }

    /// A path read for its presence says nothing of what is in it, and read
    /// for what is in it too is kept for both.
    #[test]
    fn presence_stamps_what_it_is_not_what_is_in_it() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("d");
        std::fs::create_dir(&d).unwrap();
        let before = Regard::Presence.stamp(&d);
        let whole = stamp(&d);
        std::fs::write(d.join("entry"), "x").unwrap();
        assert_eq!(Regard::Presence.stamp(&d), before, "entries come and go");
        assert_ne!(stamp(&d), whole, "a whole stamp sees them");
        assert!(matches!(before, Stamp::Present(p) if p.kind == Kind::Dir));

        std::fs::remove_file(d.join("entry")).unwrap();
        std::fs::remove_dir(&d).unwrap();
        assert_eq!(
            Regard::Presence.stamp(&d),
            Stamp::Absent,
            "gone is a change"
        );
        std::fs::write(&d, "").unwrap();
        assert!(
            matches!(Regard::Presence.stamp(&d), Stamp::Present(p) if p.kind == Kind::File),
            "and so is another kind"
        );

        let touched = Touched::default();
        touched.keep([Touch::presence(&d)]);
        touched.touch(&d);
        let regards: Vec<_> = touched.touches().iter().map(|t| t.regard).collect();
        assert_eq!(regards, [Regard::Presence, Regard::Whole]);
    }

    /// Past the cap a path is counted and not stamped.
    #[test]
    fn touched_past_the_cap_counts_and_does_not_stamp() {
        let touched = Touched::default();
        for i in 0..MOST_TOUCHED + 36 {
            touched.touch(Path::new(&format!("/nonexistent-comb/{i}")));
        }
        touched.touch(Path::new("/nonexistent-comb/0"));
        assert_eq!(touched.touches().len(), MOST_TOUCHED);
        assert_eq!(touched.count(), MOST_TOUCHED + 36);
        assert_eq!(
            touched.touches().last().unwrap().path,
            Path::new(&format!("/nonexistent-comb/{}", MOST_TOUCHED - 1))
        );
    }

    #[test]
    fn a_missing_path_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let e = lstat(&dir.path().join("nope")).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
    }
}
