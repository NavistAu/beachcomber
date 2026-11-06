//! Puts kept across a daemon restart, best effort (`comb put --persist`).
//!
//! The daemon keeps one entry per put address (its key and directory) in
//! `puts.json` in its cache directory ([`crate::config::Config::puts_path`]),
//! with the value as put and, for a put with a ttl, the wall-clock time it
//! expires. The file is rewritten whole, through a temporary file and a
//! rename, after each change. On start the daemon replays the live entries as
//! the puts they were ([`PutStore::open`]); a missing file is nothing to
//! replay, and an unreadable or invalid one is a warning and is replaced on
//! the next write. The cache directory may be cleared at any time, so this is
//! a convenience, never a guarantee.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;

/// One persisted put: what replays it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Persisted {
    /// The address's key, canonical: `namespace` or `namespace.node…`.
    pub key: String,
    /// The directory, as the put stored it; absent for the global entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub data: serde_json::Value,
    /// When a ttl'd put expires, in milliseconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<u64>,
}

impl Persisted {
    /// The whole seconds of ttl left at `now_ms`, rounded up: `None` for a put
    /// with no ttl, `Some(None)` for one already expired.
    pub fn remaining_secs(&self, now_ms: u64) -> Option<Option<u64>> {
        self.expires.map(|at| {
            at.checked_sub(now_ms)
                .filter(|&ms| ms > 0)
                .map(|ms| ms.div_ceil(1000))
        })
    }

    /// Whether this entry's address is `key` at `path`, or lies below it.
    fn at_or_below(&self, key: &str, path: Option<&str>) -> bool {
        self.path.as_deref() == path && libbeachcomber::key::within(&self.key, key)
    }
}

/// The daemon's persisted puts and the file they live in.
pub struct PutStore {
    file: PathBuf,
    entries: Mutex<Vec<Persisted>>,
}

/// Milliseconds since the Unix epoch, now.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl PutStore {
    /// A store writing to `file`, empty, and the entries `file` holds that
    /// have not expired, in the order they were put. The caller replays them
    /// through the put path, which records each again.
    pub fn open(file: PathBuf) -> (Self, Vec<Persisted>) {
        let entries = match std::fs::read(&file) {
            Ok(bytes) => match serde_json::from_slice::<Vec<Persisted>>(&bytes) {
                Ok(entries) => entries,
                Err(e) => {
                    warn!("persisted puts {} ignored: {e}", file.display());
                    Vec::new()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                warn!("persisted puts {} ignored: {e}", file.display());
                Vec::new()
            }
        };
        let now = now_ms();
        let live = entries
            .into_iter()
            .filter(|e| e.remaining_secs(now) != Some(None))
            .collect();
        let store = Self {
            file,
            entries: Mutex::new(Vec::new()),
        };
        (store, live)
    }

    /// A put to `key` at `path` landed: it replaces what stood there, so every
    /// entry at or below the address goes, and `kept` (a persisted put's
    /// entry) takes its place. The file is written when anything changed.
    pub fn record(&self, key: &str, path: Option<&str>, kept: Option<Persisted>) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let before = entries.len();
        entries.retain(|e| !e.at_or_below(key, path));
        let changed = entries.len() != before || kept.is_some();
        entries.extend(kept);
        if changed {
            self.write(&entries);
        }
    }

    /// The put at `key` at `path` expired: its entry goes, and only its own.
    pub fn expire(&self, key: &str, path: Option<&str>) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let before = entries.len();
        entries.retain(|e| !(e.key == key && e.path.as_deref() == path));
        if entries.len() != before {
            self.write(&entries);
        }
    }

    /// Write `entries` to the file through a temporary beside it and a rename,
    /// so a reader never sees half a file. A failure is logged: the puts stand
    /// in the cache either way.
    fn write(&self, entries: &[Persisted]) {
        let tmp = self
            .file
            .with_extension(format!("json.{}.tmp", std::process::id()));
        let written = (|| {
            if let Some(dir) = self.file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&tmp, serde_json::to_vec(entries)?)?;
            std::fs::rename(&tmp, &self.file)
        })();
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            warn!(
                "could not write persisted puts {}: {e}",
                self.file.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, path: Option<&str>) -> Persisted {
        Persisted {
            key: key.into(),
            path: path.map(str::to_string),
            data: serde_json::json!(1),
            expires: None,
        }
    }

    fn read(file: &std::path::Path) -> Vec<Persisted> {
        serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap()
    }

    #[test]
    fn a_put_replaces_the_entries_at_and_below_its_address_only() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("k/beachcomber/puts.json");
        let (store, _) = PutStore::open(file.clone());
        for (key, path) in [
            ("ns.a.b", None),
            ("ns.ab", None),
            ("ns.a", Some("/r")),
            ("ns", None),
        ] {
            store.record(key, path, Some(entry(key, path)));
        }
        // `ns` replaced `ns.a.b` and `ns.ab`, but not `ns.a` at another directory.
        assert_eq!(
            read(&file),
            vec![entry("ns.a", Some("/r")), entry("ns", None)]
        );
        store.record("ns.a", Some("/r"), None);
        store.expire("ns.x", None);
        assert_eq!(read(&file), vec![entry("ns", None)]);
        store.expire("ns", None);
        assert_eq!(read(&file), Vec::<Persisted>::new());
    }

    #[test]
    fn open_drops_expired_entries_and_ignores_an_invalid_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("puts.json");
        let now = now_ms();
        let live = Persisted {
            expires: Some(now + 90_500),
            ..entry("ns.live", None)
        };
        let dead = Persisted {
            expires: Some(now - 1),
            ..entry("ns.dead", None)
        };
        std::fs::write(&file, serde_json::to_vec(&[&live, &dead]).unwrap()).unwrap();
        let (_, entries) = PutStore::open(file.clone());
        assert_eq!(entries, vec![live.clone()]);
        assert_eq!(live.remaining_secs(now), Some(Some(91)));

        std::fs::write(&file, b"{not json").unwrap();
        let (store, entries) = PutStore::open(file.clone());
        assert!(entries.is_empty());
        store.record("ns", None, Some(entry("ns", None)));
        assert_eq!(read(&file), vec![entry("ns", None)]);
    }
}
