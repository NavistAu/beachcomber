//! Stat-stamp validity: what a read checks of a record before answering from it.

use super::*;
use libbeachcomber::fs::Touch;
use std::sync::Arc;
use tokio::time::Instant;

/// The file events of one instance whose Source watches the tree below its
/// directory ([`crate::provider::watches_tree`]). Nothing stats that tree, so
/// a change in it reaches a record through here: a run notes the count before
/// it begins, and a record whose count has moved is invalid.
#[derive(Debug, Default)]
pub struct TreeEvents {
    pub(super) changes: std::sync::atomic::AtomicU64,
    /// No longer watched: what the record's run left unnoted, nothing says.
    pub(super) gone: std::sync::atomic::AtomicBool,
}

/// [`TreeEvents`] as a run found them.
#[derive(Debug, Clone)]
pub struct TreeStamp {
    pub(super) events: Arc<TreeEvents>,
    pub(super) changes: u64,
}

impl TreeStamp {
    pub(super) fn moved(&self) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        self.events.gone.load(SeqCst) || self.events.changes.load(SeqCst) != self.changes
    }
}

impl PartialEq for TreeStamp {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.events, &other.events) && self.changes == other.changes
    }
}

impl Eq for TreeStamp {}

/// What a read checks of a Source record before answering from it: its stat
/// inputs as the run that made it found them. Those are the files
/// [`crate::provider::declared_inputs`] names, stamped immediately before the
/// run began, and the files its lines read, each stamped immediately before
/// it was read ([`Stamped::and_read`]). A change during the run therefore
/// fails the next check: one more run, never a stale answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Validity {
    pub(super) inputs: Arc<[(std::path::PathBuf, libbeachcomber::fs::Regard)]>,
    /// The tree's file events as the run found them, for a Source that
    /// watches its tree.
    pub(super) tree: Option<TreeStamp>,
    /// [`libbeachcomber::validity::stamps_hash`] of the inputs.
    pub(super) hash: u64,
    /// An input lies on a mount whose stamps are not trusted (network, FUSE,
    /// unknown): a read never stats it, and the check is Unknown.
    remote: bool,
    /// A stamp was taken so soon after an input's mtime that a same-size
    /// write in place could leave it unchanged (the mount's resolution, and
    /// at least 10 ms): the first check from this moment is Invalid, once
    /// the run it makes takes a stamp that is not.
    racy_until: Option<Instant>,
}

/// The least a stamp must trail its file's mtime by to be trusted: a
/// filesystem's clock is coarser than its nanosecond field.
const RACY: std::time::Duration = std::time::Duration::from_millis(10);

/// Stat inputs, each stamped before what reads it ran ([`Touch`]):
/// [`Validity::of`] makes the validity of what is kept of them.
#[derive(Debug, Clone)]
pub struct Stamped {
    /// The wall clock and the clock records are kept by, read together: how a
    /// stamp's racy window, which is on the wall clock, becomes a moment on
    /// the other.
    wall: std::time::SystemTime,
    pub(super) now: Instant,
    pub(super) stamps: Vec<Touch>,
    pub(super) tree: Option<TreeStamp>,
}

impl Stamped {
    /// Stamp `inputs` now.
    pub fn take(inputs: Vec<std::path::PathBuf>) -> Self {
        Self::of(inputs.iter().map(|path| Touch::take(path)).collect())
    }

    /// Stamps already taken, each before its file was read.
    pub fn of(stamps: Vec<Touch>) -> Self {
        Self {
            wall: std::time::SystemTime::now(),
            now: Instant::now(),
            stamps,
            tree: None,
        }
    }

    /// These, and the tree's file events as they stand.
    pub fn with_tree(mut self, tree: Option<TreeStamp>) -> Self {
        self.tree = tree;
        self
    }

    /// [`Self::take`] as though at the wall-clock moment `taken`, which is
    /// `now` on the clock records are kept by.
    #[cfg(test)]
    fn take_at(
        inputs: Vec<std::path::PathBuf>,
        taken: std::time::SystemTime,
        now: Instant,
    ) -> Self {
        let stamps = inputs
            .iter()
            .map(|path| Touch {
                at: taken,
                ..Touch::take(path)
            })
            .collect();
        Self {
            wall: taken,
            now,
            stamps,
            tree: None,
        }
    }

    /// Keep the first `n` inputs only.
    pub fn truncate(&mut self, n: usize) {
        self.stamps.truncate(n);
    }

    /// These, then `read`: what a run's lines read after it stamped its
    /// declared inputs. A path in both keeps its stamp here, the earlier.
    pub fn and_read(mut self, read: Vec<Touch>) -> Self {
        let mut seen: std::collections::HashSet<_> = self
            .stamps
            .iter()
            .map(|t| (t.path.clone(), t.regard))
            .collect();
        self.stamps.extend(
            read.into_iter()
                .filter(|t| seen.insert((t.path.clone(), t.regard))),
        );
        self
    }
}

impl Validity {
    /// Stamp `inputs` now: `None` when there are none.
    pub fn take(inputs: Vec<std::path::PathBuf>) -> Option<Self> {
        Self::of(Stamped::take(inputs))
    }

    /// The validity of `stamped`: `None` when there are none.
    pub fn of(stamped: Stamped) -> Option<Self> {
        use libbeachcomber::fs::{Mount, Stamp, mount_of};
        let Stamped {
            wall,
            now,
            stamps,
            tree,
        } = stamped;
        if stamps.is_empty() && tree.is_none() {
            return None;
        }
        let mut remote = false;
        // Until when, on the wall clock, the stamps stay racy: the latest of
        // each racy input's mtime plus its mount's resolution (and at least
        // `RACY`). A stamp is racy when it was taken before that moment.
        let mut racy: Option<std::time::SystemTime> = None;
        for touch in &stamps {
            let Stamp::Present(p) = &touch.stamp else {
                continue;
            };
            match mount_of(&touch.path, p.dev) {
                (Mount::Remote { kind }, first) => {
                    remote = true;
                    if first {
                        tracing::warn!(
                            "{} lies on a mount ({}) whose stamps a read does not take: \
                             records that read it are checked only for their lifetime",
                            touch.path.display(),
                            kind.as_deref().unwrap_or("unknown")
                        );
                    }
                }
                (Mount::Local { resolution }, _) => {
                    let mtime = std::time::UNIX_EPOCH
                        + std::time::Duration::new(
                            u64::try_from(p.mtime_secs).unwrap_or(0),
                            p.mtime_nanos,
                        );
                    let trusted = mtime + resolution.max(RACY);
                    if touch.at < trusted {
                        racy = Some(racy.map_or(trusted, |r| r.max(trusted)));
                    }
                }
            }
        }
        let hashed: Vec<(&std::path::PathBuf, Stamp)> =
            stamps.iter().map(|t| (&t.path, t.stamp)).collect();
        let hash = libbeachcomber::validity::stamps_hash(&hashed);
        Some(Self {
            hash,
            remote,
            racy_until: racy.map(|until| now + until.duration_since(wall).unwrap_or_default()),
            inputs: stamps.into_iter().map(|t| (t.path, t.regard)).collect(),
            tree,
        })
    }

    /// The stored hash, as `:cache` shows it.
    pub fn hash(&self) -> u64 {
        self.hash
    }

    /// Stamp the inputs again and compare: [`Check::Invalid`] when one
    /// changed, or the stamps were racy and their window has passed;
    /// [`Check::Unknown`] when one cannot be stamped or lies on a mount whose
    /// stamps are not trusted.
    ///
    /// [`Check::Invalid`]: libbeachcomber::validity::Check::Invalid
    /// [`Check::Unknown`]: libbeachcomber::validity::Check::Unknown
    pub fn check(&self) -> libbeachcomber::validity::Check {
        self.check_at(Instant::now())
    }

    fn check_at(&self, now: Instant) -> libbeachcomber::validity::Check {
        use libbeachcomber::fs::Stamp;
        use libbeachcomber::validity::Check;
        if self.tree.as_ref().is_some_and(TreeStamp::moved) {
            return Check::Invalid;
        }
        if self.remote {
            return Check::Unknown;
        }
        let mut stamped = Vec::with_capacity(self.inputs.len());
        for (path, regard) in self.inputs.iter() {
            let s = regard.stamp(path);
            if s == Stamp::Unknown {
                return Check::Unknown;
            }
            stamped.push((path, s));
        }
        if libbeachcomber::validity::stamps_hash(&stamped) != self.hash
            || self.racy_until.is_some_and(|until| now >= until)
        {
            Check::Invalid
        } else {
            Check::Valid
        }
    }
}

impl Cache {
    fn tree_key(provider: &str, path: Option<&str>, source: &str) -> CacheKey {
        make_cache_key(&format!("{provider}\0{source}"), path)
    }

    /// The instance's tree is watched from now on. A record from when it was
    /// not says nothing of what changed meanwhile: it is invalid.
    pub fn tree_watched(&self, provider: &str, path: Option<&str>, source: &str) {
        let key = Self::tree_key(provider, path, source);
        if let Some(old) = self.trees.insert(key, Arc::default()) {
            old.gone.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// The instance's tree is no longer watched. Nothing will say it changed,
    /// so no record of it is valid until it is watched again.
    pub fn tree_unwatched(&self, provider: &str, path: Option<&str>, source: &str) {
        if let Some(events) = self.trees.get(&Self::tree_key(provider, path, source)) {
            events.gone.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// The instance is gone, its records with it.
    pub fn tree_forget(&self, provider: &str, path: Option<&str>, source: &str) {
        self.trees.remove(&Self::tree_key(provider, path, source));
    }

    /// Something changed in the instance's tree.
    pub fn tree_changed(&self, provider: &str, path: Option<&str>, source: &str) {
        if let Some(events) = self.trees.get(&Self::tree_key(provider, path, source)) {
            events
                .changes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// A scheduler serves this cache: what a run finds of a tree that is not
    /// watched yet says nothing of what comes before the watch. Told before
    /// anything can read, so a first read is not taken for one with no watcher.
    pub fn trees_watchable(&self) {
        self.trees_watchable
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// The instance's file events as they stand, for a run about to begin.
    /// With no scheduler there is nothing to stamp; with one, a tree not yet
    /// watched has a stamp that has already moved.
    pub fn tree_stamp(
        &self,
        provider: &str,
        path: Option<&str>,
        source: &str,
    ) -> Option<TreeStamp> {
        use std::sync::atomic::Ordering::SeqCst;
        if !self.trees_watchable.load(SeqCst) {
            return None;
        }
        let events = match self.trees.get(&Self::tree_key(provider, path, source)) {
            Some(events) => Arc::clone(&events),
            None => Arc::new(TreeEvents {
                changes: Default::default(),
                gone: true.into(),
            }),
        };
        let changes = events.changes.load(SeqCst);
        Some(TreeStamp { events, changes })
    }

    /// Whether events of this instance's tree can arrive.
    pub fn tree_is_watched(&self, provider: &str, path: Option<&str>, source: &str) -> bool {
        self.trees
            .get(&Self::tree_key(provider, path, source))
            .is_some_and(|e| !e.gone.load(std::sync::atomic::Ordering::SeqCst))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::cache::test_support::*;
    use crate::provider::Value;
    use std::collections::HashMap;

    use libbeachcomber::validity::Check;

    fn validity_now(cache: &Cache) -> Option<Validity> {
        Validity::of(Stamped::of(Vec::new()).with_tree(cache.tree_stamp("g", Some("/r"), "s")))
    }

    #[test]
    fn with_no_scheduler_there_is_nothing_to_stamp() {
        let cache = Cache::new();
        cache.tree_watched("g", Some("/r"), "s");
        assert!(validity_now(&cache).is_none());
    }

    #[test]
    fn a_change_in_the_tree_invalidates_what_a_run_stamped_before_it() {
        let cache = Cache::new();
        cache.trees_watchable();
        cache.tree_watched("g", Some("/r"), "s");
        let before = validity_now(&cache).expect("a stamp");
        assert_eq!(before.check(), Check::Valid);
        cache.tree_changed("g", Some("/r"), "s");
        assert_eq!(before.check(), Check::Invalid);
        assert_eq!(validity_now(&cache).expect("a stamp").check(), Check::Valid);
    }

    /// Before the watch, and while it is gone (decay), a run says nothing of
    /// what it did not hear, however young its record is.
    #[test]
    fn a_record_made_unwatched_is_never_valid() {
        let cache = Cache::new();
        cache.trees_watchable();
        let first = validity_now(&cache).expect("a stamp");
        assert_eq!(first.check(), Check::Invalid);
        cache.tree_watched("g", Some("/r"), "s");
        assert_eq!(first.check(), Check::Invalid);
        assert!(cache.tree_is_watched("g", Some("/r"), "s"));
        let watched = validity_now(&cache).expect("a stamp");
        assert_eq!(watched.check(), Check::Valid);

        cache.tree_unwatched("g", Some("/r"), "s");
        assert!(!cache.tree_is_watched("g", Some("/r"), "s"));
        assert_eq!(watched.check(), Check::Invalid);
        let in_decay = validity_now(&cache).expect("a stamp");
        assert_eq!(in_decay.check(), Check::Invalid);

        cache.tree_watched("g", Some("/r"), "s");
        assert_eq!(in_decay.check(), Check::Invalid);
    }

    use std::time::{Duration, SystemTime};

    /// Write `body` to `path` in place and give it `mtime`: the same inode, and
    /// (for a body of the same size) the same stamp.
    fn rewrite(path: &std::path::Path, body: &str, mtime: SystemTime) {
        std::fs::write(path, body).unwrap();
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
    }

    fn mtime(path: &std::path::Path) -> SystemTime {
        std::fs::metadata(path).unwrap().modified().unwrap()
    }

    /// `paths` stamped now, as a run's lines stamp what they read.
    fn touches(paths: &[&std::path::Path]) -> Vec<Touch> {
        paths.iter().map(|p| Touch::take(p)).collect()
    }

    #[test]
    fn a_record_with_no_inputs_has_no_validity() {
        assert!(Validity::take(Vec::new()).is_none());
        assert!(Validity::of(Stamped::take(Vec::new()).and_read(Vec::new())).is_none());
    }

    #[test]
    fn a_change_to_an_input_is_invalid_and_none_is_valid() {
        let dir = tempfile::tempdir().unwrap();
        let (f, absent) = (dir.path().join("f"), dir.path().join("later"));
        let old = SystemTime::now() - Duration::from_secs(10);
        rewrite(&f, "1", old);
        let v = Validity::take(vec![f.clone(), absent.clone()]).unwrap();
        assert!(v.racy_until.is_none(), "an old mtime is not racy");
        assert_eq!(v.check(), Check::Valid);
        assert_eq!(v.check(), Check::Valid, "checking changes nothing");
        rewrite(&f, "22", old);
        assert_eq!(v.check(), Check::Invalid, "a new size");
        let v = Validity::take(vec![f.clone(), absent.clone()]).unwrap();
        std::fs::write(&absent, "").unwrap();
        assert_eq!(v.check(), Check::Invalid, "an absent input created");
    }

    /// A run's validity is its declared inputs, then what its lines read: a
    /// change to either is a change, and a path both declared and read keeps
    /// the stamp it was declared with, taken before the run began.
    #[test]
    fn a_runs_validity_is_what_it_declared_then_what_it_read() {
        let dir = tempfile::tempdir().unwrap();
        let (declared, read) = (dir.path().join("declared"), dir.path().join("read"));
        let old = SystemTime::now() - Duration::from_secs(10);
        rewrite(&declared, "1", old);
        rewrite(&read, "1", old);
        let early = Stamped::take(vec![declared.clone()]);
        // Read during the run: the declared file once more, changed since its
        // declared stamp, and one more besides.
        rewrite(&declared, "22", old);
        let v = Validity::of(early.and_read(touches(&[&read, &declared]))).unwrap();
        let inputs: Vec<_> = v.inputs.iter().map(|(path, _)| path.clone()).collect();
        assert_eq!(inputs, [declared.clone(), read.clone()]);
        assert_eq!(
            v.check(),
            Check::Invalid,
            "the declared stamp, taken before the change, is the one kept"
        );
        let v = Validity::of(Stamped::take(vec![declared.clone()]).and_read(touches(&[&read])))
            .unwrap();
        assert_eq!(v.check(), Check::Valid);
        rewrite(&read, "333", old);
        assert_eq!(v.check(), Check::Invalid, "a file the lines read changed");
        let only_read = Validity::of(Stamped::of(touches(&[&read]))).unwrap();
        assert_eq!(only_read.check(), Check::Valid);
    }

    /// Each stamp's racy window runs from when it was taken, so a file the
    /// lines read late in a run is judged by its own stamp's moment.
    #[test]
    fn a_read_stamp_is_racy_by_its_own_moment() {
        let dir = tempfile::tempdir().unwrap();
        let (early, late) = (dir.path().join("early"), dir.path().join("late"));
        rewrite(&early, "1", SystemTime::now() - Duration::from_secs(10));
        std::fs::write(&late, "1").unwrap();
        let written = mtime(&late);
        let now = Instant::now();
        let mut stamps = vec![Touch {
            at: written + Duration::from_millis(2),
            ..Touch::take(&late)
        }];
        stamps.extend(touches(&[&early]));
        let stamped = Stamped {
            tree: None,
            wall: written + Duration::from_millis(4),
            now,
            stamps,
        };
        let v = Validity::of(stamped).unwrap();
        assert_eq!(v.racy_until, Some(now + Duration::from_millis(6)));
    }

    /// A stamp taken within its mount's resolution of the file's mtime (and
    /// at least 10 ms) cannot see a same-size write in place in that window:
    /// the record is answered while the window lasts and is invalid from the
    /// moment it closes.
    #[test]
    fn a_racy_stamp_is_invalid_once_its_window_closes() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f");
        std::fs::write(&f, "1").unwrap();
        let written = mtime(&f);
        let now = Instant::now();
        let v = Validity::of(Stamped::take_at(
            vec![f.clone()],
            written + Duration::from_millis(2),
            now,
        ))
        .unwrap();
        assert_eq!(v.racy_until, Some(now + Duration::from_millis(8)));
        rewrite(&f, "2", written);
        assert_eq!(
            v.check_at(now + Duration::from_millis(7)),
            Check::Valid,
            "the stamp cannot see it"
        );
        assert_eq!(v.check_at(now + Duration::from_millis(8)), Check::Invalid);

        // A stamp taken past the window is not racy.
        let v = Validity::of(Stamped::take_at(vec![f.clone()], written + RACY, now)).unwrap();
        assert_eq!(v.racy_until, None);
        // An mtime ahead of the clock keeps the stamp racy until it has
        // passed, by the window.
        let v = Validity::of(Stamped::take_at(
            vec![f],
            written - Duration::from_secs(1),
            now,
        ))
        .unwrap();
        assert_eq!(v.racy_until, Some(now + Duration::from_secs(1) + RACY));
    }

    #[test]
    fn an_input_on_a_mount_whose_stamps_are_not_trusted_is_unknown() {
        let v = Validity {
            tree: None,
            inputs: vec![(
                std::path::PathBuf::from("/nfs/f"),
                libbeachcomber::fs::Regard::Whole,
            )]
            .into(),
            hash: 0,
            remote: true,
            racy_until: None,
        };
        assert_eq!(v.check(), Check::Unknown);
    }

    /// T4.8: validity costs a record at most 64 bytes, beside its input paths.
    #[test]
    fn validity_takes_at_most_64_bytes() {
        assert!(
            std::mem::size_of::<Option<Validity>>() <= 64,
            "{}",
            std::mem::size_of::<Option<Validity>>()
        );
    }

    /// What a read finds of a record: its validity outranks its lifetime.
    #[tokio::test(start_paused = true)]
    async fn the_state_of_a_record() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f");
        rewrite(&f, "1", SystemTime::now() - Duration::from_secs(10));
        let cache = Cache::new();
        assert_eq!(cache.record_state("p", None, "s"), RecordState::Cold);
        let schedule =
            Schedule::from_strategy(&crate::provider::InvalidationStrategy::WatchAndPoll {
                patterns: Vec::new(),
                abs_paths: vec![f.to_string_lossy().into_owned()],
                interval_secs: 60,
                count: 1,
            });
        let fields = HashMap::from([("v".to_string(), Value::Int(1))]);
        cache.put_source(
            "p",
            None,
            "s",
            fields,
            schedule.with_validity(Validity::take(vec![f.clone()])),
        );
        assert_eq!(cache.record_state("p", None, "s"), RecordState::Valid);
        let keys = cache.stored_keys("p", None, "s", "v").unwrap();
        assert!(
            matches!(&keys.get("hash"), Some(Value::String(h)) if h.len() == 16
                && h.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())),
            "{keys:?}"
        );
        tokio::time::advance(Duration::from_secs(61)).await;
        assert_eq!(cache.record_state("p", None, "s"), RecordState::Stale);
        rewrite(&f, "22", SystemTime::now());
        assert_eq!(cache.record_state("p", None, "s"), RecordState::Invalid);
    }
}
