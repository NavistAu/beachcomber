use crate::provider::Value;
use crate::provider::registry::ProviderRegistry;
use crate::watcher_registry::WatcherRegistry;
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::time::Instant;

type CacheKey = String;

/// Source label written by the `put` op. A field carrying this label has no
/// Source instance behind it — it is static, stored data (canon
/// `provider_source.md` "Field": "a static label (`put`, or `config:<file>`
/// for a leaf seeded from that configuration file)").
pub const PUT_SOURCE: &str = "put";

/// Prefix on a source label naming a leaf seeded from a configuration file
/// (canon `provider_source.md` "Field"). A future task writes sources named
/// `config:<file>`; this constant names the prefix `Cache::list_rows` checks.
pub const CONFIG_SOURCE_PREFIX: &str = "config:";

/// Whether a source name is a static label — one with no Source instance
/// behind it (canon `provider_source.md` "Field"). `put` and `config:<file>`
/// are the two; everything else names a Source the daemon refreshes.
fn is_static_label(source_name: &str) -> bool {
    source_name == PUT_SOURCE || source_name.starts_with(CONFIG_SOURCE_PREFIX)
}

/// Why a static write was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PutError {
    /// The record is produced by one of the provider's Sources, so the next
    /// refresh would overwrite it. Records are refused one at a time — a
    /// namespace is never refused wholesale.
    Owned { record: String, source: String },
    /// The root of a namespace is a map of records, not a single value.
    RootNotObject,
    /// An object key with a `.` in it names one record but addresses another.
    DottedKey { key: String },
}

impl std::fmt::Display for PutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PutError::Owned { record, source } => {
                write!(f, "cannot write {record}: owned by source {source}")
            }
            PutError::RootNotObject => write!(f, "put data at the root must be an object"),
            PutError::DottedKey { key } => {
                write!(f, "put data keys may not contain '.': {key}")
            }
        }
    }
}

impl std::error::Error for PutError {}

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
    /// Merge every source's fields into one tree, recursing into objects the
    /// maps share. The maps hold disjoint *records*, so no value is ever
    /// decided by merge order — only the intermediate objects two maps both
    /// contribute to get stitched together.
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
    // The literal-key probe mirrors `lookup_path`, which a Source's output may
    // still need; `remove_path` has no matching case, but `put` cannot create a
    // literal dotted key (see `PutError::DottedKey`), so the two never disagree
    // about a static record.
    if fields.contains_key(tail) {
        return true;
    }
    let mut current: Option<&Value> = None;
    for segment in tail.split('.') {
        current = match current {
            None => fields.get(segment),
            Some(Value::Object(map)) => map.get(segment),
            // A non-object ancestor: this map holds what stands at the address.
            Some(_) => return true,
        };
        if current.is_none() {
            return false;
        }
    }
    true
}

/// Every record a value writes, as `(tail below the namespace, leaf value)`.
///
/// A record is a leaf: any non-object, plus an empty object (which stores an
/// empty node rather than nothing). `prefix` is the node the value is written
/// at — empty for the namespace root, where the value is always an object and
/// its members are what get walked.
fn walk_leaves(prefix: &str, value: Value, out: &mut Vec<(String, Value)>) {
    match value {
        Value::Object(map) if !map.is_empty() || prefix.is_empty() => {
            for (k, v) in map {
                let child = if prefix.is_empty() {
                    k
                } else {
                    format!("{prefix}.{k}")
                };
                walk_leaves(&child, v, out);
            }
        }
        other => out.push((prefix.to_string(), other)),
    }
}

/// The first object key containing a `.`, if any.
///
/// A dotted key names one record and addresses another: `{"a.b": 1}` would be
/// stored as `a` → `{b: 1}`, indistinguishable from `put ns.a '{"b":1}'` and
/// unreachable by the name it was written under. `put` refuses it rather than
/// silently reinterpreting it.
fn dotted_key(value: &Value) -> Option<&str> {
    let Value::Object(map) = value else {
        return None;
    };
    for (k, v) in map {
        if k.contains('.') {
            return Some(k);
        }
        if let Some(found) = dotted_key(v) {
            return Some(found);
        }
    }
    None
}

/// The Source that closes this whole namespace to `put`, if there is one.
///
/// A Source whose field names are only known once it runs declares a sentinel
/// (`<tool>`, `<profile>`, `<field>` — see `ProviderMetadata::validate`) rather
/// than a name a key could match, so no key can be shown *not* to belong to it:
/// the namespace is closed to `put`. The rule has to hold on a cold entry too,
/// or a `put` lands first and the Source's next refresh writes the same record
/// into its own map, leaving two records at one (key, path). The first sentinel
/// Source in declaration order names the error.
fn sentinel_source<'a>(registry: &'a ProviderRegistry, namespace: &str) -> Option<&'a str> {
    registry
        .provider_sources(namespace)?
        .iter()
        .find(|sm| sm.fields.iter().any(|f| f.name.starts_with('<')))
        .map(|sm| sm.name.as_str())
}

/// The Source that owns the record at `tail` under `namespace`, if any.
///
/// A record is Source-owned when the registry declares its head segment as a
/// field of one of the provider's Sources — that is what the next refresh
/// overwrites — when it currently sits in a source map the registry knows, or
/// when `sentinel` says the namespace is closed to `put` at all. `put` and
/// `config:<file>` are never in the registry, so a static record never comes
/// back from here.
fn source_owner(
    registry: &ProviderRegistry,
    namespace: &str,
    entry: Option<&CacheEntry>,
    tail: &str,
    sentinel: Option<&str>,
) -> Option<String> {
    let head = tail.split('.').next().unwrap_or(tail);
    if let Some(source) = registry.source_for_field(namespace, head) {
        return Some(source.to_string());
    }
    if let Some(owner) = entry.and_then(|e| e.owner_of(tail))
        && registry.source(namespace, owner).is_some()
    {
        return Some(owner.to_string());
    }
    sentinel.map(str::to_string)
}

/// The first of `tails` a Source owns, as the error refusing the write.
fn refuse_owned<'t>(
    registry: &ProviderRegistry,
    namespace: &str,
    entry: Option<&CacheEntry>,
    tails: impl Iterator<Item = &'t str>,
) -> Option<PutError> {
    let sentinel = sentinel_source(registry, namespace);
    for tail in tails {
        if let Some(source) = source_owner(registry, namespace, entry, tail, sentinel) {
            return Some(PutError::Owned {
                record: format!("{namespace}.{tail}"),
                source,
            });
        }
    }
    None
}

/// Drop the subtree at `node` (`""` for everything) from every static source
/// map, pruning any map it empties except `keep` — the label a write is about
/// to land in. Source maps are never touched: a `put` does not remove a record
/// a Source produces. Returns whether anything was removed.
fn drop_static_subtree(entry: &mut CacheEntry, node: &str, keep: Option<&str>) -> bool {
    let mut removed = false;
    entry.sources.retain(|name, src| {
        if !is_static_label(name) {
            return true;
        }
        if node.is_empty() {
            removed |= !src.fields.is_empty();
            src.fields.clear();
        } else {
            removed |= crate::provider::remove_path(&mut src.fields, node).is_some();
        }
        !src.fields.is_empty() || keep == Some(name.as_str())
    });
    removed
}

/// Apply one static write to an entry already under a write guard.
fn apply_static_write(
    entry: &mut CacheEntry,
    label: &str,
    node: &str,
    leaves: Vec<(String, Value)>,
    replace: bool,
    interval_secs: Option<u64>,
    now: Instant,
) {
    if replace {
        // The label's map is kept only when there is something to fill it with;
        // otherwise it would survive the drop and never be written, leaving a
        // phantom empty static map (and, at the root, an entry the caller's
        // emptiness check would no longer remove).
        drop_static_subtree(entry, node, (!leaves.is_empty()).then_some(label));
    }

    // Rule 1: one record, one map. Only a static map ever gives a record up —
    // the caller's ownership scan ran under this same guard, so no refresh can
    // have slipped in behind it, and `is_static_label` makes that structural.
    for (tail, _) in &leaves {
        let owner = entry.owner_of(tail).map(str::to_string);
        if let Some(owner) = owner
            && owner != label
            && is_static_label(&owner)
            && let Some(src) = entry.sources.get_mut(&owner)
        {
            remove_occupant(&mut src.fields, tail);
            if src.fields.is_empty() {
                entry.sources.remove(&owner);
            }
        }
    }

    if leaves.is_empty() {
        return;
    }
    let src = entry
        .sources
        .entry(label.to_string())
        .or_insert_with(|| CacheSourceEntry {
            fields: HashMap::new(),
            last_refreshed: now,
            expected_interval_secs: interval_secs,
        });
    for (tail, v) in leaves {
        crate::provider::insert_path(&mut src.fields, &tail, v);
    }
    src.last_refreshed = now;
    if interval_secs.is_some() {
        src.expected_interval_secs = interval_secs;
    }
}

/// Remove whatever occupies the address `tail` in this map: the node itself,
/// or the non-object ancestor standing in its place. Only called for a map
/// [`CacheEntry::owner_of`] named, so one of the two always exists.
fn remove_occupant(fields: &mut HashMap<String, Value>, tail: &str) {
    let mut cut = tail;
    loop {
        if crate::provider::remove_path(fields, cut).is_some() {
            return;
        }
        match cut.rfind('.') {
            Some(i) => cut = &cut[..i],
            None => return,
        }
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

    /// Write static data at `node`, the dotted tail below `namespace` (`""` for
    /// the namespace root), under the static source label `label`.
    ///
    /// Every leaf of `value` becomes one record. Records deep-merge into
    /// whatever is already stored; `replace` first drops the whole subtree at
    /// `node` from every *static* map — a Source's records are never removed by
    /// a `put`. A record a Source produces is refused — the next refresh would
    /// overwrite it — and nothing is written.
    ///
    /// A record lives in exactly one source map, so a write first evicts the
    /// record from whichever map holds it. Reads therefore never need a
    /// precedence rule between labels.
    ///
    /// The ownership scan and the write run under one write guard. Scanning
    /// under a read guard and mutating under a later one would let a Source
    /// refresh land in between, so the eviction could drop a record the scan
    /// had cleared as static.
    #[allow(clippy::too_many_arguments)]
    pub fn write_static(
        &self,
        namespace: &str,
        path: Option<&str>,
        label: &str,
        node: &str,
        value: Value,
        replace: bool,
        interval_secs: Option<u64>,
        registry: &ProviderRegistry,
    ) -> Result<(), PutError> {
        if node.is_empty() && !matches!(value, Value::Object(_)) {
            return Err(PutError::RootNotObject);
        }
        if let Some(key) = dotted_key(&value) {
            return Err(PutError::DottedKey {
                key: key.to_string(),
            });
        }
        let mut leaves = Vec::new();
        walk_leaves(node, value, &mut leaves);
        let key = make_cache_key(namespace, path);
        let now = Instant::now();

        // The records this write claims are its own leaves. `replace` drops the
        // static subtree at `node` as well, but a Source's records under it are
        // left where they are, so they are not part of the scan — otherwise
        // `put uptime {"note":2} --replace` on a warm namespace would be
        // refused for records it never touches.
        match self.entries.entry(key) {
            dashmap::Entry::Occupied(mut occupied) => {
                if let Some(err) = refuse_owned(
                    registry,
                    namespace,
                    Some(occupied.get()),
                    leaves.iter().map(|(tail, _)| tail.as_str()),
                ) {
                    return Err(err);
                }
                apply_static_write(
                    occupied.get_mut(),
                    label,
                    node,
                    leaves,
                    replace,
                    interval_secs,
                    now,
                );
                if occupied.get().sources.is_empty() {
                    occupied.remove();
                }
            }
            dashmap::Entry::Vacant(vacant) => {
                // Nothing cached, so only the registry can refuse — and
                // dropping the vacant entry leaves the slot untouched.
                if let Some(err) = refuse_owned(
                    registry,
                    namespace,
                    None,
                    leaves.iter().map(|(tail, _)| tail.as_str()),
                ) {
                    return Err(err);
                }
                // A write with no leaves (`put ns '{}'`) has nothing to store
                // and, on a slot this empty, nothing to drop either: it must
                // not conjure an entry.
                if leaves.is_empty() {
                    return Ok(());
                }
                let mut fresh = CacheEntry {
                    sources: HashMap::new(),
                    created_at: now,
                };
                apply_static_write(&mut fresh, label, node, leaves, replace, interval_secs, now);
                vacant.insert(fresh);
            }
        }

        if let Some(ref watchers) = self.watchers {
            watchers.notify(namespace, path);
        }
        Ok(())
    }

    /// Remove the static subtree at `node` — the dotted tail below `namespace`,
    /// `""` for the namespace root, where every static record of the entry
    /// goes. Source-produced records are never removed; addressing one
    /// directly refuses the removal instead of silently doing nothing. The
    /// entry is dropped when no source remains.
    ///
    /// One write guard covers the scan and the removal, as in `write_static`.
    ///
    /// Returns whether anything was removed.
    pub fn remove_static_path(
        &self,
        namespace: &str,
        path: Option<&str>,
        node: &str,
        registry: &ProviderRegistry,
    ) -> Result<bool, PutError> {
        let key = make_cache_key(namespace, path);
        // Only the addressed node is scanned: a Source's records beneath it are
        // left in place, exactly as `replace` leaves them.
        let scan = |entry: Option<&CacheEntry>| -> Option<PutError> {
            if node.is_empty() {
                return None;
            }
            refuse_owned(registry, namespace, entry, std::iter::once(node))
        };

        let mut removed = false;
        match self.entries.entry(key) {
            dashmap::Entry::Occupied(mut occupied) => {
                if let Some(err) = scan(Some(occupied.get())) {
                    return Err(err);
                }
                removed = drop_static_subtree(occupied.get_mut(), node, None);
                if occupied.get().sources.is_empty() {
                    occupied.remove();
                }
            }
            dashmap::Entry::Vacant(_) => {
                if let Some(err) = scan(None) {
                    return Err(err);
                }
            }
        }

        if removed && let Some(ref watchers) = self.watchers {
            watchers.notify(namespace, path);
        }
        Ok(removed)
    }

    /// Look up a specific field across all sources for (provider, path).
    /// Returns the field value from whichever source owns it, or `None` if not found.
    ///
    /// `field` may be a dotted sub-path (e.g. `"project.rust"`): the first segment is the
    /// top-level field name and subsequent segments traverse nested `Value::Object` maps.
    ///
    /// Returns the value plus the owning source's `last_refreshed` timestamp so callers
    /// can report per-field freshness (canon §"Field freshness": each field's age is the
    /// freshness of its owning Source's last successful refresh).
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

    /// Whether the `(provider, path)` slot holds an entry, without cloning it.
    pub fn has_entry(&self, provider: &str, path: Option<&str>) -> bool {
        self.entries.contains_key(&make_cache_key(provider, path))
    }

    pub fn remove(&self, provider: &str, path: Option<&str>) {
        let key = make_cache_key(provider, path);
        self.entries.remove(&key);
    }

    /// Remove a single source's contribution from the (provider, path) entry.
    /// If that was the last source, the whole entry is removed. No-op if the
    /// entry or the named source is absent — in which case no watcher
    /// notification is emitted (nothing changed).
    pub fn remove_source(&self, provider: &str, path: Option<&str>, source: &str) {
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
            for (source_name, src) in &ce.sources {
                let age = src.age_ms();
                let stale = src.is_stale();
                // A row whose source is the synthetic `put` label or a
                // `config:<file>` seed carries no Source instance — it is a
                // static entry (canon `provider_source.md` "Cache entry").
                let kind = if is_static_label(source_name) {
                    Some(RowKind::Static)
                } else {
                    None
                };
                for (field, v) in &src.fields {
                    // Value implements Serialize with #[serde(untagged)] — to_value is lossless.
                    let value = serde_json::to_value(v).unwrap_or(serde_json::Value::Null);
                    out.push(CacheRow {
                        provider: provider.clone(),
                        path: path.clone(),
                        source: source_name.clone(),
                        field: field.clone(),
                        value,
                        age_ms: age,
                        stale,
                        kind: kind.clone(),
                        poll_interval_secs: None,
                        keep_alive_polls: None,
                        fsevents_reinstate: None,
                        polls_elapsed: None,
                        next_poll_in_secs: None,
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
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CacheEntryInfo {
    pub provider: String,
    pub path: Option<String>,
    pub age_ms: u128,
    pub stale: bool,
    pub field_count: usize,
}

/// One row in the `Request::Status` tabular response — one per (provider, path, source, field).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CacheRow {
    pub provider: String,
    pub path: Option<String>,
    /// Name of the Source that produced this field. Drives per-row lifecycle
    /// snapshot lookup so the renderer can format glyphs and TTL columns
    /// according to the owning Source's strategy, not the provider's.
    pub source: String,
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
    /// Seconds until this source's next scheduled poll. `None` for pure Watch
    /// sources (no poll path) or when the lifecycle registry has no entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_poll_in_secs: Option<u64>,
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
    Static,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_fields(pairs: &[(&str, &str)]) -> HashMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
            .collect()
    }

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
            None,
        );
        cache.put_source(
            "git",
            Some("/repo"),
            "extras",
            make_fields(&[("dirty", "false")]),
            None,
        );

        // Overwrite only "base" with new data.
        cache.put_source(
            "git",
            Some("/repo"),
            "base",
            make_fields(&[("branch", "feat/x"), ("sha", "def456")]),
            None,
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
            None,
        );
        cache.put_source(
            "git",
            Some("/repo"),
            "diff",
            make_fields(&[("dirty", "true")]),
            None,
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
            None,
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
            None,
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
            None,
        );
        cache.put_source(
            "git",
            Some("/repo"),
            "extras",
            make_fields(&[("dirty", "true")]),
            None,
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

    /// A row whose source is seeded from a config file (`config:<file>`)
    /// carries no Source instance — `list_rows` must mark it `RowKind::Static`,
    /// same as a `put`-written row.
    #[test]
    fn list_rows_marks_config_seeded_source_static() {
        let cache = Cache::new();
        cache.put_source(
            "myapp",
            None,
            "config:x.toml",
            make_fields(&[("greeting", "hello")]),
            None,
        );

        let rows = cache.list_rows();
        let row = rows
            .iter()
            .find(|r| r.provider == "myapp")
            .expect("row for myapp must be present");
        assert_eq!(
            row.kind,
            Some(RowKind::Static),
            "config-seeded row must be RowKind::Static, got {:?}",
            row.kind
        );
    }

    // ── static labels beside provider sources ────────────────────────────────

    /// A source label is just a `CacheSourceEntry` name, so a `put` record and a
    /// Source's records share one entry. Evicting the Source's map leaves the
    /// static record — and only the static record — behind.
    #[test]
    fn static_record_survives_its_entry_mate_being_evicted() {
        let cache = Cache::new();
        let registry = crate::provider::registry::ProviderRegistry::with_defaults();

        cache.put_source(
            "git",
            Some("/repo"),
            "head",
            make_fields(&[("branch", "main")]),
            None,
        );
        cache
            .write_static(
                "git",
                Some("/repo"),
                PUT_SOURCE,
                "note",
                Value::Int(1),
                false,
                None,
                &registry,
            )
            .expect("note is declared by no source");

        cache.remove_source("git", Some("/repo"), "head");

        let entry = cache
            .get_entry("git", Some("/repo"))
            .expect("entry survives the head source being evicted");
        let flat = entry.flatten_fields();
        assert_eq!(
            flat,
            HashMap::from([("note".to_string(), Value::Int(1))]),
            "only the static record remains"
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
            },
        );
        sources.insert(
            "head".to_string(),
            CacheSourceEntry {
                fields: make_fields(&[("branch", "main")]),
                last_refreshed: Instant::now(),
                expected_interval_secs: None,
            },
        );
        let entry = CacheEntry {
            sources,
            created_at: Instant::now(),
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
                },
            );
        }
        let entry = CacheEntry {
            sources,
            created_at: Instant::now(),
        };

        let mut expected = HashMap::new();
        crate::provider::insert_path(&mut expected, "tree.left", Value::Int(1));
        crate::provider::insert_path(&mut expected, "tree.right", Value::Int(2));
        assert_eq!(entry.flatten_fields(), expected);
    }
}
