use crate::provider::ProviderResult;
use dashmap::DashMap;
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
}

impl Cache {
    pub fn new() -> Self {
        Self {
            entries: DashMap::new(),
            generation: std::sync::atomic::AtomicU64::new(0),
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
        let current_gen = self.generation.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.entries.insert(key, CacheEntry {
            result,
            created_at: Instant::now(),
            generation: current_gen,
            expected_interval_secs: interval_secs,
        });
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

    /// List all cache entries with their keys parsed back into (provider, path) and age info.
    pub fn list_entries(&self) -> Vec<CacheEntryInfo> {
        self.entries.iter().map(|entry| {
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
        }).collect()
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

impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}
