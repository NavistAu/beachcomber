//! Storage: the `Cache` and its entry types.

use super::*;
use crate::provider::Value;
use crate::watcher_registry::WatcherRegistry;
use dashmap::DashMap;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use tokio::time::Instant;

pub(super) type CacheKey = String;

/// What a read finds of one instance's record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordState {
    /// Nothing is stored: the read runs the Source and waits for the run.
    Cold,
    /// Within its lifetime and its inputs as they were: answered as it is.
    Valid,
    /// Past its lifetime (`stale`).
    Stale,
    /// An input changed since the run that made it.
    Invalid,
}

/// The namespace holding the `:cache` facet of `namespace`.
pub(super) fn cache_facet_of(namespace: &str) -> String {
    format!("{namespace}:cache")
}

/// The namespace holding the `:memo` facet of `namespace`.
pub(super) fn memo_facet_of(namespace: &str) -> String {
    format!("{namespace}:memo")
}

/// Whether two canonical addresses overlap: one is the other, or lies below it.
pub(crate) fn overlaps(a: &str, b: &str) -> bool {
    libbeachcomber::key::within(a, b) || libbeachcomber::key::within(b, a)
}

/// Whether `tail` is at or below the `_` prototype node of a `:cache` facet.
pub(super) fn is_prototype(tail: &str) -> bool {
    tail == "_" || tail.starts_with("_.")
}

/// Build a compact cache key from a provider name and optional path.
/// Uses a null byte as separator since it cannot appear in valid paths.
pub(super) fn make_cache_key(provider: &str, path: Option<&str>) -> CacheKey {
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
    /// What a read checks before answering from a Source's record; `None`
    /// for a static record and a Source with no stat inputs.
    pub validity: Option<Validity>,
}

impl CacheSourceEntry {
    /// An empty static map, written at `now`: its records' freshness is
    /// their stamps' to own.
    pub(super) fn static_at(now: Instant) -> Self {
        Self {
            fields: HashMap::new(),
            last_refreshed: now,
            expected_interval_secs: None,
            validity: None,
        }
    }

    pub fn age_ms(&self) -> u128 {
        self.last_refreshed.elapsed().as_millis()
    }

    /// Returns true if the source is older than its expected refresh interval.
    pub fn is_stale(&self) -> bool {
        match self.expected_interval_secs {
            Some(interval) => {
                self.last_refreshed.elapsed() > std::time::Duration::from_secs(interval)
            }
            None => false,
        }
    }
}

/// Where one record came from: the name of the source map it was read from,
/// and that map's freshness reduced to the two numbers a response reports. A
/// record's freshness is its own — never the entry's, and never the chain's.
#[derive(Debug, Clone)]
pub struct RecordFreshness {
    /// The record's one writer: a Source of the namespace's provider (`head`,
    /// `refs`, …), or a static label (`put`, `config:<file>`). This is what
    /// the `source` of `get <p>:cache.<key>`. Shared rather than owned: one source map
    /// contributes many records and they all name it, so the label is
    /// allocated once per map and refcounted per record.
    pub source: Arc<str>,
    /// The directory entry the record's map lives at (`None`: the global
    /// entry) — where the `:cache` facet reads the keys stored beside it.
    pub entry: Option<Arc<str>>,
    pub last_refreshed: Instant,
    pub expected_interval_secs: Option<u64>,
}

impl RecordFreshness {
    pub fn age_ms(&self) -> u128 {
        self.last_refreshed.elapsed().as_millis()
    }

    pub fn is_stale(&self) -> bool {
        match self.expected_interval_secs {
            Some(interval) => {
                self.last_refreshed.elapsed() > std::time::Duration::from_secs(interval)
            }
            None => false,
        }
    }
}

/// When one put or memo address was written, and its ttl, if any.
#[derive(Debug, Clone, PartialEq)]
pub struct PutStamp {
    pub written: Instant,
    pub interval_secs: Option<u64>,
}

impl PutStamp {
    pub fn age_ms(&self) -> u128 {
        self.written.elapsed().as_millis()
    }

    pub fn is_stale(&self) -> bool {
        self.interval_secs.is_some_and(|interval| {
            self.written.elapsed() > std::time::Duration::from_secs(interval)
        })
    }
}

/// A complete cache entry for a (provider, path) key. Holds one `CacheSourceEntry`
/// per named source. Sources are disjoint by field name — validated at registration time.
#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub sources: HashMap<String, CacheSourceEntry>,
    /// One stamp per address a `put` wrote at (`""` for the namespace root).
    /// The `put` map in `sources` holds the values; this holds when each
    /// address was written and how long it is valid for.
    pub puts: BTreeMap<String, PutStamp>,
    /// On a `<p>:memo` entry, one stamp per template leaf with a memo here
    /// (its canonical address): the memo's record. Its value, when it has
    /// one, is under [`MEMO_SOURCE`] in `sources`; a memo of absence has the
    /// stamp alone. Its stored keys are in the `<p>:cache` entry at the same
    /// path.
    pub memos: BTreeMap<String, PutStamp>,
    pub created_at: Instant,
    /// The `config:<file>` labels of this entry in the order their files
    /// compose — `defaults/`, then the main file, then `conf.d/` — which is
    /// the order they are written in. Each file keeps its own records, so two
    /// may hold the same one; a read takes the later file's.
    pub config_order: Vec<String>,
}

impl CacheEntry {
    /// An entry holding nothing yet, made at `now`.
    pub(super) fn empty(now: Instant) -> Self {
        Self {
            sources: HashMap::new(),
            puts: BTreeMap::new(),
            memos: BTreeMap::new(),
            created_at: now,
            config_order: Vec::new(),
        }
    }

    /// Merge every source's fields into one tree, recursing into objects the
    /// maps share. Not a read: two `config:` maps may hold the same record,
    /// and only [`Cache::read_merged`] stacks them in composition order.
    pub fn flatten_fields(&self) -> HashMap<String, Value> {
        let mut out = HashMap::new();
        for src in self.sources.values() {
            crate::provider::deep_merge(&mut out, src.fields.clone());
        }
        out
    }

    /// Name of the source map occupying the address `tail`, or `None` if no map
    /// does. A map occupies the address when it holds the node itself, or when
    /// it holds a non-object on an ancestor segment — that scalar is what
    /// stands in the way of the address below it.
    ///
    /// An empty tail names the entry root, which no single map owns: `None`.
    pub fn owner_of(&self, tail: &str) -> Option<&str> {
        if tail.is_empty() {
            return None;
        }
        self.sources
            .iter()
            .find(|(_, src)| occupies(&src.fields, tail))
            .map(|(name, _)| name.as_str())
    }

    /// The `config:<file>` maps this entry holds, in composition order: the
    /// order [`Self::config_order`] records, and any map it does not name
    /// after those, by name.
    pub(super) fn config_labels(&self) -> Vec<&String> {
        let rank = |name: &String| {
            self.config_order
                .iter()
                .position(|l| l == name)
                .unwrap_or(usize::MAX)
        };
        let mut labels: Vec<&String> = self
            .sources
            .keys()
            .filter(|name| name.starts_with(CONFIG_SOURCE_PREFIX))
            .collect();
        labels.sort_by(|a, b| rank(a).cmp(&rank(b)).then(a.cmp(b)));
        labels
    }

    /// The stamp governing the record at `tail`: the put address at or above
    /// it, nearest first.
    pub(super) fn put_stamp_for(&self, tail: &str) -> Option<&PutStamp> {
        self.puts.get(self.put_address_for(tail)?)
    }

    /// The put address governing the record at `tail`: at or above it, nearest
    /// first.
    pub(super) fn put_address_for(&self, tail: &str) -> Option<&str> {
        let mut at = tail.to_string();
        loop {
            if let Some((addr, _)) = self.puts.get_key_value(&at) {
                return Some(addr.as_str());
            }
            at = libbeachcomber::key::parent(&at)?;
        }
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

/// Whether this field map occupies the address `tail` — see [`CacheEntry::owner_of`].
fn occupies(fields: &HashMap<String, Value>, tail: &str) -> bool {
    let mut current: Option<&Value> = None;
    for segment in libbeachcomber::key::segments(tail) {
        current = match current {
            None => fields.get(segment.as_ref()),
            Some(Value::Object(map)) => map.get(segment.as_ref()),
            // A non-object ancestor: this map holds what stands at the address.
            Some(_) => return true,
        };
        if current.is_none() {
            return false;
        }
    }
    true
}

/// Whether the put address `addr` is `node` or lies below it (`""` is the
/// namespace root, which every address is at or below).
pub(crate) fn is_at_or_below(addr: &str, node: &str) -> bool {
    node.is_empty()
        || addr == node
        || addr
            .strip_prefix(node)
            .is_some_and(|rest| rest.starts_with('.'))
}

pub struct Cache {
    pub(super) entries: DashMap<CacheKey, CacheEntry>,
    pub(super) watchers: Option<Arc<WatcherRegistry>>,
    /// The directories each provider has a memo at: where an invalidation
    /// looks for the memos it drops the hashes of. A write to a provider with
    /// none is one lookup. The eviction of a directory's last memo unlists it
    /// ([`Cache::evict_memo`]), and a provider's last directory the provider.
    pub(super) memo_paths: DashMap<String, std::collections::HashSet<Option<String>>>,
    /// The file events of each instance whose tree is watched now.
    pub(super) trees: DashMap<CacheKey, Arc<TreeEvents>>,
    pub(super) trees_watchable: std::sync::atomic::AtomicBool,
    /// How a read knows the file events of before it have arrived; set by a
    /// scheduler whose watcher delivers in order.
    barrier: std::sync::OnceLock<Arc<crate::watcher::Barrier>>,
    /// Where persisted puts are kept; set by a daemon that keeps them.
    pub(super) put_store: std::sync::OnceLock<crate::persist::PutStore>,
}

impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}

/// Reading a whole entry or one field back out of the cache, for the tests
/// that check what a run stored. Nothing in the daemon reads this way: it
/// reads through [`Cache::read_merged`], which layers and merges.
#[cfg(any(test, feature = "test-helpers"))]
impl Cache {
    /// Look up a specific field across all sources for (provider, path).
    /// Returns the field value from whichever source owns it, or `None` if not found.
    ///
    /// `field` may be a dotted sub-path (e.g. `"project.rust"`): the first segment is the
    /// top-level field name and subsequent segments traverse nested `Value::Object` maps.
    ///
    /// Returns the value plus the owning source's `last_refreshed` timestamp so callers
    /// can report per-field freshness (each field's age is the freshness of its
    /// owning Source's last successful refresh).
    pub fn get_field(
        &self,
        provider: &str,
        path: Option<&str>,
        field: &str,
    ) -> Option<(Value, Instant)> {
        let key = make_cache_key(provider, path);
        let entry = self.entries.get(&key)?;
        for src in entry.sources.values() {
            if let Some(value) = crate::provider::lookup_path(&src.fields, field) {
                return Some((value.clone(), src.last_refreshed));
            }
        }
        None
    }

    /// Return a clone of the full `CacheEntry` for (provider, path), or `None`.
    /// Replaces the old `get()`.
    pub fn get_entry(&self, provider: &str, path: Option<&str>) -> Option<CacheEntry> {
        let key = make_cache_key(provider, path);
        self.entries.get(&key).map(|e| e.clone())
    }
}

impl Cache {
    pub fn new() -> Self {
        Self {
            entries: DashMap::new(),
            watchers: None,
            memo_paths: DashMap::new(),
            trees: DashMap::new(),
            trees_watchable: Default::default(),
            barrier: std::sync::OnceLock::new(),
            put_store: std::sync::OnceLock::new(),
        }
    }

    pub fn with_watchers(watchers: Arc<WatcherRegistry>) -> Self {
        Self {
            entries: DashMap::new(),
            watchers: Some(watchers),
            memo_paths: DashMap::new(),
            trees: DashMap::new(),
            trees_watchable: Default::default(),
            barrier: std::sync::OnceLock::new(),
            put_store: std::sync::OnceLock::new(),
        }
    }

    pub fn set_barrier(&self, barrier: Arc<crate::watcher::Barrier>) {
        let _ = self.barrier.set(barrier);
    }

    pub fn barrier(&self) -> Option<&Arc<crate::watcher::Barrier>> {
        self.barrier.get()
    }

    /// Write a single source's result into the cache entry for (provider, path).
    /// Other sources at the same (provider, path) are not touched.
    /// Creates the entry if absent; updates only `source_name`'s record if present.
    ///
    /// The `schedule` is written into the `<provider>:cache` entry at the same
    /// path, under the same label, one object per top-level field, before the
    /// record itself: a reader that sees the record sees its schedule. One
    /// notification, the record's.
    pub fn put_source(
        &self,
        provider: &str,
        path: Option<&str>,
        source_name: &str,
        fields: HashMap<String, Value>,
        schedule: Schedule,
    ) {
        let now = Instant::now();
        let stored = schedule.stored_keys();
        if !stored.is_empty() {
            let facet_key = make_cache_key(&cache_facet_of(provider), path);
            // A record rewritten with the schedule it already carries only has
            // its clock moved.
            let unchanged = self
                .entries
                .get_mut(&facet_key)
                .and_then(|mut entry| {
                    let held = entry.sources.get_mut(source_name)?;
                    let same = held.fields.len() == fields.len()
                        && fields.keys().all(|f| {
                            matches!(held.fields.get(f), Some(Value::Object(o)) if *o == stored)
                        });
                    if same {
                        held.last_refreshed = now;
                    }
                    Some(same)
                })
                .unwrap_or(false);
            if !unchanged {
                let per_field = fields
                    .keys()
                    .map(|f| (f.clone(), Value::Object(stored.clone())))
                    .collect();
                self.insert_source_map(
                    facet_key,
                    source_name,
                    CacheSourceEntry {
                        fields: per_field,
                        last_refreshed: now,
                        expected_interval_secs: None,
                        validity: None,
                    },
                );
            }
        }
        let written: Vec<String> = if self.memo_paths.contains_key(provider) {
            fields
                .keys()
                .map(|f| libbeachcomber::key::quote(f).into_owned())
                .collect()
        } else {
            Vec::new()
        };
        self.insert_source_map(
            make_cache_key(provider, path),
            source_name,
            CacheSourceEntry {
                fields,
                last_refreshed: now,
                expected_interval_secs: schedule.poll.map(|(secs, _)| secs),
                validity: schedule.validity,
            },
        );
        if let Some(ref watchers) = self.watchers {
            watchers.notify(provider, path);
        }
        for field in written {
            self.drop_memo_hashes_under(provider, &field);
        }
    }

    pub(super) fn insert_source_map(
        &self,
        key: CacheKey,
        source_name: &str,
        src: CacheSourceEntry,
    ) {
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
                puts: BTreeMap::new(),
                memos: BTreeMap::new(),
                created_at: Instant::now(),
                config_order: Vec::new(),
            });
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

    /// The keys stored beside the record at `tail` of `provider`'s entry `at`,
    /// written by `label`: `poll`, `watch` and `decay` as its writer stored
    /// them. A Source stores them per top-level field; a put stores them at
    /// its address, so a leaf takes those of the nearest put address at or
    /// above it. `None` when the record has none.
    pub fn stored_keys(
        &self,
        provider: &str,
        at: Option<&str>,
        label: &str,
        tail: &str,
    ) -> Option<HashMap<String, Value>> {
        if label.starts_with(CONFIG_SOURCE_PREFIX) {
            return None;
        }
        let head = libbeachcomber::key::segments(tail).first().map_or_else(
            || tail.to_string(),
            |h| libbeachcomber::key::quote(h).into_owned(),
        );
        let address = if label == PUT_SOURCE {
            let entry = self.entries.get(&make_cache_key(provider, at))?;
            let address = entry.put_address_for(tail)?.to_string();
            if address.is_empty() {
                head.to_string()
            } else {
                address
            }
        } else {
            head.to_string()
        };
        let facet = self
            .entries
            .get(&make_cache_key(&cache_facet_of(provider), at))?;
        match crate::provider::lookup_path(&facet.sources.get(label)?.fields, &address)? {
            Value::Object(keys) => Some(keys.clone()),
            _ => None,
        }
    }

    /// How many decay steps a Source of `provider` at `path` takes before
    /// eviction: the `_.decay` prototype, found like any value — up the
    /// directory chain from `path`, nearest wins, and at one directory
    /// `<provider>:cache` beats `*:cache` — else
    /// [`DEFAULT_DECAY`](crate::scheduler::lifecycle::DEFAULT_DECAY).
    pub fn resolve_decay(&self, provider: &str, path: Option<&str>) -> u8 {
        let namespaces = [cache_facet_of(provider), "*:cache".to_string()];
        crate::query::path_chain(path)
            .iter()
            .find_map(|layer| {
                namespaces.iter().find_map(|namespace| {
                    let entry = self
                        .entries
                        .get(&make_cache_key(namespace, layer.as_deref()))?;
                    let mut out = MergedRead {
                        fields: HashMap::new(),
                        records: HashMap::new(),
                    };
                    Self::merge_static_records(&entry, None, &mut out);
                    match crate::provider::lookup_path(&out.fields, "_.decay")? {
                        Value::Int(n)
                            if (1..=crate::scheduler::lifecycle::MAX_DECAY).contains(n) =>
                        {
                            u8::try_from(*n).ok()
                        }
                        _ => None,
                    }
                })
            })
            .unwrap_or(crate::scheduler::lifecycle::DEFAULT_DECAY)
    }

    /// Whether the entry at `(provider, path)` holds a map written by `source`.
    ///
    /// Coldness is per Source, not per provider: a sibling Source being warm
    /// says nothing about this one, and neither does a static record — that is
    /// not the provider's own data, one inherited from up the chain least of
    /// all.
    pub fn has_provider_records_for(
        &self,
        provider: &str,
        path: Option<&str>,
        source: &str,
    ) -> bool {
        self.entries
            .get(&make_cache_key(provider, path))
            .is_some_and(|e| e.sources.contains_key(source))
    }

    /// What a read finds of the record `source` wrote at `(provider, path)`:
    /// none, or one past its lifetime, or one whose stat inputs changed since
    /// its run (checked now, by stat), or one it can answer from. An input
    /// that changed outranks the lifetime: it needs a run that sees the
    /// change.
    ///
    /// The state is per Source: each record carries the lifetime and the
    /// inputs of its own Source, and a sibling that is overdue or changed says
    /// nothing about this one.
    pub fn record_state(&self, provider: &str, path: Option<&str>, source: &str) -> RecordState {
        let (stale, validity) = {
            let Some(entry) = self.entries.get(&make_cache_key(provider, path)) else {
                return RecordState::Cold;
            };
            let Some(record) = entry.sources.get(source) else {
                return RecordState::Cold;
            };
            (record.is_stale(), record.validity.clone())
        };
        // Stamped with no lock held: a stat is a syscall.
        if validity.is_some_and(|v| v.check() == libbeachcomber::validity::Check::Invalid) {
            RecordState::Invalid
        } else if stale {
            RecordState::Stale
        } else {
            RecordState::Valid
        }
    }

    /// Remove a single source's contribution from the (provider, path) entry.
    /// If that was the last source, the whole entry is removed. No-op if the
    /// entry or the named source is absent — in which case no watcher
    /// notification is emitted (nothing changed).
    pub fn remove_source(&self, provider: &str, path: Option<&str>, source: &str) {
        // Its stored `:cache` keys go with it; they describe nothing else.
        if !is_static_label(source) {
            let facet_key = make_cache_key(&cache_facet_of(provider), path);
            if let Some(mut entry) = self.entries.get_mut(&facet_key) {
                entry.sources.remove(source);
            }
            self.entries
                .remove_if(&facet_key, |_, entry| entry.sources.is_empty());
        }

        let key = make_cache_key(provider, path);
        // Drop the source under the shard lock, recording whether anything
        // actually changed. The `get_mut` guard must be released before the
        // `remove_if` below or DashMap deadlocks on the same shard.
        let mut changed = false;
        if let Some(mut entry) = self.entries.get_mut(&key) {
            changed = entry.sources.remove(source).is_some();
        }
        if changed {
            // Drop the entry only if it is *still* empty when re-checked under
            // the shard lock — `remove_if` guards against a concurrent
            // `put_source` repopulating it in the window after the guard above
            // was released (it won't remove a non-empty entry).
            self.entries
                .remove_if(&key, |_, entry| entry.sources.is_empty());
            if let Some(ref watchers) = self.watchers {
                watchers.notify(provider, path);
            }
        }
    }

    /// Remove `source`'s contribution at every path it is stored at, whether or
    /// not the scheduler keeps a lifecycle entry for the instance.
    pub fn remove_source_everywhere(&self, provider: &str, source: &str) {
        let paths: Vec<Option<String>> = self
            .entries
            .iter()
            .filter(|entry| entry.value().sources.contains_key(source))
            .filter_map(|entry| {
                let mut parts = entry.key().splitn(2, '\0');
                (parts.next() == Some(provider)).then(|| parts.next().map(str::to_string))
            })
            .collect();
        for path in paths {
            self.remove_source(provider, path.as_deref(), source);
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether the cache holds nothing but what the daemon seeded from its
    /// own configuration (`config:<file>` maps): no Source record, put or
    /// memo. Idle shutdown asks this; a seed alone does not keep a daemon up.
    pub fn holds_only_config_seeds(&self) -> bool {
        self.entries.iter().all(|entry| {
            entry.puts.is_empty()
                && entry.memos.is_empty()
                && entry
                    .sources
                    .keys()
                    .all(|label| label.starts_with(CONFIG_SOURCE_PREFIX))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::cache::test_support::*;

    /// B2: Writing source "A" must not touch any fields owned by source "B"
    /// at the same (provider, path) key.
    #[test]
    fn put_source_overwrites_only_named_source() {
        let cache = Cache::new();

        // Populate two disjoint sources.
        cache.put_source(
            "git",
            Some("/repo"),
            "base",
            make_fields(&[("branch", "main"), ("sha", "abc123")]),
            crate::cache::Schedule::default(),
        );
        cache.put_source(
            "git",
            Some("/repo"),
            "extras",
            make_fields(&[("dirty", "false")]),
            crate::cache::Schedule::default(),
        );

        // Overwrite only "base" with new data.
        cache.put_source(
            "git",
            Some("/repo"),
            "base",
            make_fields(&[("branch", "feat/x"), ("sha", "def456")]),
            crate::cache::Schedule::default(),
        );

        // "extras" source must be untouched.
        let extra_src = cache
            .get_source("git", Some("/repo"), "extras")
            .expect("extras source must still exist");
        assert_eq!(
            extra_src.fields.get("dirty").unwrap().as_text(),
            "false",
            "extras.dirty must be unchanged after overwriting base"
        );

        // "base" source must reflect the new values.
        let base_src = cache
            .get_source("git", Some("/repo"), "base")
            .expect("base source must exist");
        assert_eq!(
            base_src.fields.get("branch").unwrap().as_text(),
            "feat/x",
            "base.branch must be updated"
        );
        assert_eq!(
            base_src.fields.get("sha").unwrap().as_text(),
            "def456",
            "base.sha must be updated"
        );
    }

    #[test]
    fn remove_source_drops_only_named_source() {
        let cache = Cache::new();
        cache.put_source(
            "git",
            Some("/repo"),
            "refs",
            make_fields(&[("branch", "main")]),
            crate::cache::Schedule::default(),
        );
        cache.put_source(
            "git",
            Some("/repo"),
            "diff",
            make_fields(&[("dirty", "true")]),
            crate::cache::Schedule::default(),
        );

        cache.remove_source("git", Some("/repo"), "refs");

        // refs gone, diff intact, entry still present.
        assert!(
            cache.get_source("git", Some("/repo"), "refs").is_none(),
            "refs removed"
        );
        let (dirty, _) = cache
            .get_field("git", Some("/repo"), "dirty")
            .expect("diff.dirty survives");
        assert_eq!(dirty.as_text(), "true");
        assert!(
            cache.get_entry("git", Some("/repo")).is_some(),
            "entry survives while diff remains"
        );
    }

    #[test]
    fn remove_source_removes_entry_when_last() {
        let cache = Cache::new();
        cache.put_source(
            "git",
            Some("/repo"),
            "refs",
            make_fields(&[("branch", "main")]),
            crate::cache::Schedule::default(),
        );

        cache.remove_source("git", Some("/repo"), "refs");

        assert!(
            cache.get_entry("git", Some("/repo")).is_none(),
            "entry removed when last source gone"
        );
    }

    #[test]
    fn remove_source_nonexistent_is_noop() {
        let cache = Cache::new();
        cache.put_source(
            "git",
            Some("/repo"),
            "refs",
            make_fields(&[("branch", "main")]),
            crate::cache::Schedule::default(),
        );
        cache.remove_source("git", Some("/repo"), "nope");
        assert!(
            cache.get_source("git", Some("/repo"), "refs").is_some(),
            "unrelated source untouched"
        );
    }

    /// B2: `get_field` must find a field regardless of which source owns it.
    #[test]
    fn get_field_routes_through_source() {
        let cache = Cache::new();

        cache.put_source(
            "git",
            Some("/repo"),
            "base",
            make_fields(&[("branch", "main")]),
            crate::cache::Schedule::default(),
        );
        cache.put_source(
            "git",
            Some("/repo"),
            "extras",
            make_fields(&[("dirty", "true")]),
            crate::cache::Schedule::default(),
        );

        // "branch" lives in "base" — get_field should find it.
        let (branch, _) = cache
            .get_field("git", Some("/repo"), "branch")
            .expect("get_field must find branch");
        assert_eq!(branch.as_text(), "main");

        // "dirty" lives in "extras" — get_field should find it too.
        let (dirty, _) = cache
            .get_field("git", Some("/repo"), "dirty")
            .expect("get_field must find dirty");
        assert_eq!(dirty.as_text(), "true");

        // A field that doesn't exist in any source returns None.
        assert!(
            cache
                .get_field("git", Some("/repo"), "nonexistent")
                .is_none(),
            "get_field must return None for unknown field"
        );
    }

    /// `owner_of` names the map holding the record — including when the record
    /// sits inside an object that map holds.
    #[test]
    fn owner_of_finds_the_map_holding_a_nested_record() {
        let mut sources = HashMap::new();
        let mut nested = HashMap::new();
        crate::provider::insert_path(&mut nested, "some.value", Value::Int(5));
        sources.insert(
            PUT_SOURCE.to_string(),
            CacheSourceEntry {
                fields: nested,
                last_refreshed: Instant::now(),
                expected_interval_secs: None,
                validity: None,
            },
        );
        sources.insert(
            "head".to_string(),
            CacheSourceEntry {
                fields: make_fields(&[("branch", "main")]),
                last_refreshed: Instant::now(),
                expected_interval_secs: None,
                validity: None,
            },
        );
        let entry = CacheEntry {
            sources,
            puts: BTreeMap::new(),
            memos: BTreeMap::new(),
            created_at: Instant::now(),
            config_order: Vec::new(),
        };

        assert_eq!(entry.owner_of("some.value"), Some(PUT_SOURCE));
        assert_eq!(entry.owner_of("some"), Some(PUT_SOURCE));
        assert_eq!(entry.owner_of("branch"), Some("head"));
        assert_eq!(entry.owner_of("nothing"), None);
        assert_eq!(entry.owner_of("some.nope"), None);
        // A scalar in the way owns the address below it.
        assert_eq!(entry.owner_of("branch.sub"), Some("head"));
    }

    /// `flatten_fields` deep-merges, so two maps contributing to the same
    /// object node both show up in a whole-entry read.
    #[test]
    fn flatten_fields_deep_merges_across_sources() {
        let mut sources = HashMap::new();
        let mut a = HashMap::new();
        crate::provider::insert_path(&mut a, "tree.left", Value::Int(1));
        let mut b = HashMap::new();
        crate::provider::insert_path(&mut b, "tree.right", Value::Int(2));
        for (name, fields) in [("one", a), ("two", b)] {
            sources.insert(
                name.to_string(),
                CacheSourceEntry {
                    fields,
                    last_refreshed: Instant::now(),
                    expected_interval_secs: None,
                    validity: None,
                },
            );
        }
        let entry = CacheEntry {
            sources,
            puts: BTreeMap::new(),
            memos: BTreeMap::new(),
            created_at: Instant::now(),
            config_order: Vec::new(),
        };

        let mut expected = HashMap::new();
        crate::provider::insert_path(&mut expected, "tree.left", Value::Int(1));
        crate::provider::insert_path(&mut expected, "tree.right", Value::Int(2));
        assert_eq!(entry.flatten_fields(), expected);
    }

    #[test]
    fn resolve_decay_prefers_the_provider_prototype() {
        let cache = Cache::new();
        assert_eq!(
            cache.resolve_decay("git", None),
            crate::scheduler::lifecycle::DEFAULT_DECAY
        );
        cache
            .write_static(
                "*:cache",
                None,
                "config:00-comb.toml",
                "_.decay",
                Value::Int(6),
                None,
            )
            .unwrap();
        assert_eq!(cache.resolve_decay("git", None), 6, "the star prototype");
        cache
            .write_static(
                "git:cache",
                None,
                PUT_SOURCE,
                "_.decay",
                Value::Int(2),
                None,
            )
            .unwrap();
        assert_eq!(cache.resolve_decay("git", None), 2, "the provider's own");
        assert_eq!(cache.resolve_decay("mise", None), 6);
    }

    #[test]
    fn resolve_decay_follows_the_directory_chain_nearest_first() {
        let cache = Cache::new();
        let put = |ns: &str, at: Option<&str>, n: i64| {
            cache
                .write_static(ns, at, PUT_SOURCE, "_.decay", Value::Int(n), None)
                .unwrap();
        };
        put("*:cache", None, 3);
        put("git:cache", Some("/w/proj"), 6);
        assert_eq!(cache.resolve_decay("git", Some("/w/proj")), 6);
        assert_eq!(cache.resolve_decay("git", Some("/w/proj/sub")), 6);
        assert_eq!(cache.resolve_decay("git", Some("/w/other")), 3);
        assert_eq!(cache.resolve_decay("git", None), 3);
        assert_eq!(cache.resolve_decay("mise", Some("/w/proj/sub")), 3);

        put("*:cache", Some("/w/proj/sub"), 5);
        assert_eq!(
            cache.resolve_decay("git", Some("/w/proj/sub/x")),
            5,
            "a nearer star beats a farther provider prototype"
        );
        put("git:cache", Some("/w/proj/sub"), 2);
        assert_eq!(cache.resolve_decay("git", Some("/w/proj/sub")), 2);
    }
}
