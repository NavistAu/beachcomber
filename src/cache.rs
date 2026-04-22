use crate::provider::ProviderResult;
use crate::watcher_registry::WatcherRegistry;
use dashmap::DashMap;
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

#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub result: ProviderResult,
    pub created_at: Instant,
    pub generation: u64,
    /// Expected refresh interval in seconds, used to compute staleness.
    /// None means staleness is never reported (e.g. Once providers).
    pub expected_interval_secs: Option<u64>,
}

impl CacheEntry {
    pub fn age_ms(&self) -> u128 {
        self.created_at.elapsed().as_millis()
    }

    /// Returns true if the entry is older than its expected refresh interval.
    pub fn is_stale(&self) -> bool {
        match self.expected_interval_secs {
            Some(interval) => self.created_at.elapsed().as_secs() > interval,
            None => false,
        }
    }
}

pub struct Cache {
    entries: DashMap<CacheKey, CacheEntry>,
    generation: std::sync::atomic::AtomicU64,
    watchers: Option<Arc<WatcherRegistry>>,
}

impl Cache {
    pub fn new() -> Self {
        Self {
            entries: DashMap::new(),
            generation: std::sync::atomic::AtomicU64::new(0),
            watchers: None,
        }
    }

    pub fn with_watchers(watchers: Arc<WatcherRegistry>) -> Self {
        Self {
            entries: DashMap::new(),
            generation: std::sync::atomic::AtomicU64::new(0),
            watchers: Some(watchers),
        }
    }

    pub fn get(&self, provider: &str, path: Option<&str>) -> Option<CacheEntry> {
        let key = make_cache_key(provider, path);
        self.entries.get(&key).map(|entry| entry.clone())
    }

    pub fn put(&self, provider: &str, path: Option<&str>, result: ProviderResult) {
        self.put_with_interval(provider, path, result, None);
    }

    pub fn put_with_interval(
        &self,
        provider: &str,
        path: Option<&str>,
        result: ProviderResult,
        interval_secs: Option<u64>,
    ) {
        let key = make_cache_key(provider, path);
        let current_gen = self
            .generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.entries.insert(
            key,
            CacheEntry {
                result,
                created_at: Instant::now(),
                generation: current_gen,
                expected_interval_secs: interval_secs,
            },
        );
        if let Some(ref watchers) = self.watchers {
            watchers.notify(provider, path);
        }
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

    /// List one row per field across all cache entries.
    /// Used by `Request::Status` to return the tabular "what is warm?" payload.
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
            let age = ce.age_ms();
            let stale = ce.is_stale();
            for (field, v) in &ce.result.fields {
                // Value implements Serialize with #[serde(untagged)] — to_value is lossless.
                let value = serde_json::to_value(v).unwrap_or(serde_json::Value::Null);
                out.push(CacheRow {
                    provider: provider.clone(),
                    path: path.clone(),
                    field: field.clone(),
                    value,
                    age_ms: age,
                    stale,
                });
            }
        }
        out
    }

    /// List all cache entries with their keys parsed back into (provider, path) and age info.
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
                let value = entry.value();
                CacheEntryInfo {
                    provider,
                    path,
                    age_ms: value.age_ms(),
                    stale: value.is_stale(),
                    field_count: value.result.fields.len(),
                }
            })
            .collect()
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
}

impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}
