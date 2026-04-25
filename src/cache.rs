use crate::provider::{ProviderResult, Value};
use crate::watcher_registry::WatcherRegistry;
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

type CacheKey = String;

/// Build a compact cache key from a provider name and optional path.
/// Uses a null byte as separator since it cannot appear in valid paths.
fn make_cache_key(provider: &str, path: Option<&str>) -> CacheKey {
    match path {
        Some(p) => format!("{provider}\0{p}"),
        None => provider.to_string(),
    }
}

/// One source's contribution to a cache entry. Tracks its own refresh timestamp
/// and staleness independently from sibling sources.
#[derive(Debug, Clone)]
pub struct CacheSourceEntry {
    pub fields: HashMap<String, Value>,
    pub last_refreshed: Instant,
    pub expected_interval_secs: Option<u64>,
}

impl CacheSourceEntry {
    pub fn age_ms(&self) -> u128 {
        self.last_refreshed.elapsed().as_millis()
    }

    /// Returns true if the source is older than its expected refresh interval.
    pub fn is_stale(&self) -> bool {
        match self.expected_interval_secs {
            Some(interval) => self.last_refreshed.elapsed().as_secs() > interval,
            None => false,
        }
    }
}

/// A complete cache entry for a (provider, path) key. Holds one `CacheSourceEntry`
/// per named source. Sources are disjoint by field name — validated at registration time.
#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub sources: HashMap<String, CacheSourceEntry>,
    pub created_at: Instant,
}

impl CacheEntry {
    /// Merge all source fields into a flat map. If two sources declare the same field
    /// (which registration-time validation prevents), last-writer wins.
    pub fn flatten_fields(&self) -> HashMap<String, Value> {
        let mut out = HashMap::new();
        for src in self.sources.values() {
            for (k, v) in &src.fields {
                out.insert(k.clone(), v.clone());
            }
        }
        out
    }

    /// Returns the `Instant` of the source that was refreshed least recently,
    /// or `None` if there are no sources yet.
    pub fn oldest_refreshed(&self) -> Option<Instant> {
        self.sources.values().map(|s| s.last_refreshed).min()
    }

    /// Age in milliseconds from the oldest source, or from `created_at` if no sources.
    /// Used by `list_entries()` to produce a single representative age for the entry.
    pub fn age_ms(&self) -> u128 {
        self.oldest_refreshed()
            .map(|t| t.elapsed().as_millis())
            .unwrap_or_else(|| self.created_at.elapsed().as_millis())
    }

    /// True if *any* source is stale.
    pub fn is_stale(&self) -> bool {
        self.sources.values().any(|s| s.is_stale())
    }
}

pub struct Cache {
    entries: DashMap<CacheKey, CacheEntry>,
    watchers: Option<Arc<WatcherRegistry>>,
}

impl Cache {
    pub fn new() -> Self {
        Self {
            entries: DashMap::new(),
            watchers: None,
        }
    }

    pub fn with_watchers(watchers: Arc<WatcherRegistry>) -> Self {
        Self {
            entries: DashMap::new(),
            watchers: Some(watchers),
        }
    }

    /// Write a single source's result into the cache entry for (provider, path).
    /// Other sources at the same (provider, path) are not touched.
    /// Creates the entry if absent; updates only `source_name`'s slot if present.
    pub fn put_source(
        &self,
        provider: &str,
        path: Option<&str>,
        source_name: &str,
        fields: HashMap<String, Value>,
        interval_secs: Option<u64>,
    ) {
        let key = make_cache_key(provider, path);
        let src = CacheSourceEntry {
            fields,
            last_refreshed: Instant::now(),
            expected_interval_secs: interval_secs,
        };
        self.entries
            .entry(key)
            .and_modify(|ce| {
                ce.sources.insert(source_name.to_string(), src.clone());
            })
            .or_insert_with(|| CacheEntry {
                sources: {
                    let mut m = HashMap::new();
                    m.insert(source_name.to_string(), src);
                    m
                },
                created_at: Instant::now(),
            });
        if let Some(ref watchers) = self.watchers {
            watchers.notify(provider, path);
        }
    }

    /// Look up a specific field across all sources for (provider, path).
    /// Returns the field value from whichever source owns it, or `None` if not found.
    pub fn get_field(&self, provider: &str, path: Option<&str>, field: &str) -> Option<Value> {
        let key = make_cache_key(provider, path);
        let entry = self.entries.get(&key)?;
        for src in entry.sources.values() {
            if let Some(v) = src.fields.get(field) {
                return Some(v.clone());
            }
        }
        None
    }

    /// Return a clone of a specific source's entry, or `None` if the (provider, path) key
    /// or the named source within it doesn't exist.
    pub fn get_source(
        &self,
        provider: &str,
        path: Option<&str>,
        source_name: &str,
    ) -> Option<CacheSourceEntry> {
        let key = make_cache_key(provider, path);
        let entry = self.entries.get(&key)?;
        entry.sources.get(source_name).cloned()
    }

    /// Return a clone of the full `CacheEntry` for (provider, path), or `None`.
    /// Replaces the old `get()`.
    pub fn get_entry(&self, provider: &str, path: Option<&str>) -> Option<CacheEntry> {
        let key = make_cache_key(provider, path);
        self.entries.get(&key).map(|e| e.clone())
    }

    pub fn remove(&self, provider: &str, path: Option<&str>) {
        let key = make_cache_key(provider, path);
        self.entries.remove(&key);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// List one row per (source, field) across all cache entries.
    /// Used by `Request::Status` to return the tabular "what is warm?" payload.
    /// Each row's `age_ms` comes from that field's owning source's last-refreshed timestamp.
    pub fn list_rows(&self) -> Vec<CacheRow> {
        let mut out = Vec::new();
        for entry in self.entries.iter() {
            let key = entry.key();
            let (provider, path) = if let Some(sep) = key.find('\0') {
                (key[..sep].to_string(), Some(key[sep + 1..].to_string()))
            } else {
                (key.clone(), None)
            };
            let ce = entry.value();
            for (_source_name, src) in &ce.sources {
                let age = src.age_ms();
                let stale = src.is_stale();
                for (field, v) in &src.fields {
                    // Value implements Serialize with #[serde(untagged)] — to_value is lossless.
                    let value = serde_json::to_value(v).unwrap_or(serde_json::Value::Null);
                    out.push(CacheRow {
                        provider: provider.clone(),
                        path: path.clone(),
                        field: field.clone(),
                        value,
                        age_ms: age,
                        stale,
                        kind: None,
                        poll_interval_secs: None,
                        keep_alive_polls: None,
                        fsevents_reinstate: None,
                        polls_elapsed: None,
                        failure: None,
                    });
                }
            }
        }
        out
    }

    /// List all cache entries with their keys parsed back into (provider, path) and age info.
    /// `field_count` is the total across all sources. `age_ms` is the oldest source's age
    /// (i.e., the least recently refreshed source), or `created_at` age if no sources present.
    pub fn list_entries(&self) -> Vec<CacheEntryInfo> {
        self.entries
            .iter()
            .map(|entry| {
                let key = entry.key();
                let (provider, path) = if let Some(sep) = key.find('\0') {
                    (key[..sep].to_string(), Some(key[sep + 1..].to_string()))
                } else {
                    (key.clone(), None)
                };
                let ce = entry.value();
                let field_count: usize = ce.sources.values().map(|s| s.fields.len()).sum();
                CacheEntryInfo {
                    provider,
                    path,
                    age_ms: ce.age_ms(),
                    stale: ce.is_stale(),
                    field_count,
                }
            })
            .collect()
    }

    /// Legacy write path: write a `ProviderResult` as a single source named `"default"`.
    /// Exists to ease migration — new code should call `put_source` directly.
    #[allow(dead_code)]
    pub fn put(&self, provider: &str, path: Option<&str>, result: ProviderResult) {
        self.put_with_interval(provider, path, result, None);
    }

    /// Legacy write path with interval: write a `ProviderResult` as source `"default"`.
    /// Exists to ease migration — new code should call `put_source` directly.
    #[allow(dead_code)]
    pub fn put_with_interval(
        &self,
        provider: &str,
        path: Option<&str>,
        result: ProviderResult,
        interval_secs: Option<u64>,
    ) {
        self.put_source(provider, path, "default", result.fields, interval_secs);
    }

    /// Legacy read path: return a `CacheEntry` shaped like the old struct.
    /// Used only by callers not yet migrated to `get_entry`. Maps sources["default"].
    #[allow(dead_code)]
    pub fn get(&self, provider: &str, path: Option<&str>) -> Option<LegacyCacheEntry> {
        let key = make_cache_key(provider, path);
        let entry = self.entries.get(&key)?;
        // Flatten across all sources for backwards compatibility.
        let mut fields = HashMap::new();
        let mut last_refreshed = None::<Instant>;
        let mut expected_interval_secs = None;
        for src in entry.sources.values() {
            for (k, v) in &src.fields {
                fields.insert(k.clone(), v.clone());
            }
            last_refreshed = Some(match last_refreshed {
                None => src.last_refreshed,
                Some(prev) => prev.min(src.last_refreshed),
            });
            if expected_interval_secs.is_none() {
                expected_interval_secs = src.expected_interval_secs;
            }
        }
        Some(LegacyCacheEntry {
            result: ProviderResult { fields },
            created_at: last_refreshed.unwrap_or(entry.created_at),
            expected_interval_secs,
        })
    }
}

/// Backwards-compatible view returned by `Cache::get()` for callers not yet migrated.
/// Presents a flattened single-source view across all sources. New callers should use
/// `get_entry()`, `get_source()`, or `get_field()` instead.
#[derive(Debug, Clone)]
pub struct LegacyCacheEntry {
    pub result: ProviderResult,
    pub created_at: Instant,
    pub expected_interval_secs: Option<u64>,
}

impl LegacyCacheEntry {
    pub fn age_ms(&self) -> u128 {
        self.created_at.elapsed().as_millis()
    }

    pub fn is_stale(&self) -> bool {
        match self.expected_interval_secs {
            Some(interval) => self.created_at.elapsed().as_secs() > interval,
            None => false,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CacheEntryInfo {
    pub provider: String,
    pub path: Option<String>,
    pub age_ms: u128,
    pub stale: bool,
    pub field_count: usize,
}

/// One row in the `Request::Status` tabular response — one per (provider, path, field).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CacheRow {
    pub provider: String,
    pub path: Option<String>,
    pub field: String,
    pub value: serde_json::Value,
    pub age_ms: u128,
    pub stale: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<RowKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_interval_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_alive_polls: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fsevents_reinstate: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polls_elapsed: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<FailureSnapshot>,
}

/// Discriminator used by the status formatter to choose rendering strategy.
/// `Lifecycle` entries get TTL countdown rendering; others render `---`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RowKind {
    Lifecycle { decay: u8, watches_files: bool },
    Once,
    Virtual,
    Transient,
}

/// Failure state for a cache entry, embedded in status rows.
/// Tracks how many consecutive execution failures have occurred and whether
/// the provider is currently suppressed (back-off in effect).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FailureSnapshot {
    pub consecutive_failures: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suppressed_until_unix_ms: Option<u64>,
}

impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}

