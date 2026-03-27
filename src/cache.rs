use crate::provider::ProviderResult;
use dashmap::DashMap;
use std::time::Instant;

type CacheKey = (String, Option<String>);

#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub result: ProviderResult,
    pub created_at: Instant,
    pub generation: u64,
}

impl CacheEntry {
    pub fn age_ms(&self) -> u128 {
        self.created_at.elapsed().as_millis()
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
        let key = (provider.to_string(), path.map(|s| s.to_string()));
        self.entries.get(&key).map(|entry| entry.clone())
    }

    pub fn put(&self, provider: &str, path: Option<&str>, result: ProviderResult) {
        let key = (provider.to_string(), path.map(|s| s.to_string()));
        let current_gen = self.generation.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.entries.insert(key, CacheEntry {
            result,
            created_at: Instant::now(),
            generation: current_gen,
        });
    }

    pub fn remove(&self, provider: &str, path: Option<&str>) {
        let key = (provider.to_string(), path.map(|s| s.to_string()));
        self.entries.remove(&key);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}
