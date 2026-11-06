//! Stamping a file's mtime, for tests that wait on a watcher noticing it.
//!
//! A watcher's poll compares mtimes, and a filesystem's mtime granularity is
//! coarse enough that a file written twice in quick succession can carry the
//! same one twice. Stamping the mtime into the future settles that, and settles
//! the start-up race with it: a watch records its baseline when its thread
//! begins, and a stamp already ahead of that baseline beats it whichever of the
//! two happened first — so a test can write and stamp the instant `spawn_*`
//! returns instead of waiting for the thread.

use std::path::Path;
use std::time::{Duration, SystemTime};

/// Stamp `path`'s mtime `secs` seconds into the future.
#[allow(dead_code)]
pub fn set_mtime_ahead(path: &Path, secs: u64) {
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() + Duration::from_secs(secs))
        .unwrap();
}

/// [`set_mtime_ahead`] with the two seconds that is enough for every watcher in
/// this suite.
#[allow(dead_code)]
pub fn touch_future(path: &Path) {
    set_mtime_ahead(path, 2);
}
