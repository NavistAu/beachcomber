use crate::provider::Value;
use crate::provider::registry::ProviderRegistry;
use crate::watcher_registry::WatcherRegistry;
use dashmap::DashMap;
use std::collections::{BTreeMap, HashMap};
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
    /// The root of a namespace is a map of records, not a single value.
    RootNotObject,
    /// An object key with a `.` in it names one record but addresses another.
    DottedKey { key: String },
}

impl std::fmt::Display for PutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
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

/// Where one record came from: the name of the source map it was read from,
/// and that map's freshness reduced to the two numbers a response reports. A
/// record's freshness is its own — never the entry's, and never the chain's
/// (canon `provider_source.md` §"Field freshness").
#[derive(Debug, Clone)]
pub struct RecordFreshness {
    /// The record's one writer: a Source of the namespace's provider (`head`,
    /// `refs`, …), or a static label (`put`, `config:<file>`). This is what
    /// `get <key>:source` reports. Shared rather than owned: one source map
    /// contributes many records and they all name it, so the label is
    /// allocated once per map and refcounted per record.
    pub source: Arc<str>,
    pub last_refreshed: Instant,
    pub expected_interval_secs: Option<u64>,
}

impl RecordFreshness {
    pub fn age_ms(&self) -> u128 {
        self.last_refreshed.elapsed().as_millis()
    }

    pub fn is_stale(&self) -> bool {
        match self.expected_interval_secs {
            Some(interval) => self.last_refreshed.elapsed().as_secs() > interval,
            None => false,
        }
    }
}

/// Which layers of a namespace a read sees. `Put` is what `put` and config
/// seeds stored; `Source` is what the provider's Sources stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layers {
    All,
    Put,
    Source,
}

/// What a chain-resolved read of one namespace found: see [`Cache::read_merged`],
/// which is the only thing that builds one.
#[derive(Debug, Clone)]
pub struct MergedRead {
    /// The merged value tree — every record the read resolved, nearest first.
    pub fields: HashMap<String, Value>,
    /// Provenance, one entry per record in `fields`, keyed by the record's
    /// dotted tail below the namespace. Keying by the tail rather than by the
    /// top-level field is what makes "which record answered this leaf" a single
    /// map lookup even when two sources contribute leaves to one object — which
    /// is exactly what the chain does, layer over layer.
    pub records: HashMap<String, RecordFreshness>,
}

/// When one put address was written, and its `--ttl`, if any.
#[derive(Debug, Clone)]
pub struct PutStamp {
    pub written: Instant,
    pub interval_secs: Option<u64>,
}

impl PutStamp {
    pub fn age_ms(&self) -> u128 {
        self.written.elapsed().as_millis()
    }

    pub fn is_stale(&self) -> bool {
        self.interval_secs
            .is_some_and(|interval| self.written.elapsed().as_secs() > interval)
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

    /// The stamp governing the record at `tail`: the put address at or above
    /// it, nearest first.
    fn put_stamp_for(&self, tail: &str) -> Option<&PutStamp> {
        let mut at = tail;
        loop {
            if let Some(stamp) = self.puts.get(at) {
                return Some(stamp);
            }
            at = match at.rfind('.') {
                Some(i) => &at[..i],
                None if at.is_empty() => return None,
                None => "",
            };
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

/// Whether the put address `addr` is `node` or lies below it (`""` is the
/// namespace root, which every address is at or below).
pub(crate) fn is_at_or_below(addr: &str, node: &str) -> bool {
    node.is_empty()
        || addr == node
        || addr
            .strip_prefix(node)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// Drop the subtree at `node` (`""` for everything) from the `put` map,
/// pruning it if it empties, unless it is `keep` — the label a write is about
/// to land in. Every put address at or below `node` goes with it, and so does
/// any address the removal left holding nothing. Every other layer — Source
/// maps and `config:` seeds — is left alone, so the layer below a removed put
/// shows through at once. Returns whether anything was removed.
fn drop_put_subtree(entry: &mut CacheEntry, node: &str, keep: Option<&str>) -> bool {
    let mut removed = false;
    entry.sources.retain(|name, src| {
        if name != PUT_SOURCE {
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
    entry.puts.retain(|addr, _| !is_at_or_below(addr, node));
    let held = entry.sources.get(PUT_SOURCE).map(|src| &src.fields);
    entry.puts.retain(|addr, _| {
        held.is_some_and(|fields| {
            if addr.is_empty() {
                !fields.is_empty()
            } else {
                crate::provider::lookup_path(fields, addr).is_some()
            }
        })
    });
    removed
}

/// Drop the leaves of the put address `node` that no deeper put address owns.
/// Each put address keeps the lifecycle its own put set, so a deeper address
/// (and its stamp) survives the expiry of one above it. Returns whether
/// anything was removed.
fn drop_put_address(entry: &mut CacheEntry, node: &str) -> bool {
    let deeper: Vec<String> = entry
        .puts
        .keys()
        .filter(|addr| *addr != node && is_at_or_below(addr, node))
        .cloned()
        .collect();
    if deeper.is_empty() {
        return drop_put_subtree(entry, node, None);
    }
    let kept: Vec<(String, Value)> = entry
        .sources
        .get(PUT_SOURCE)
        .map(|src| {
            deeper
                .iter()
                .filter_map(|addr| {
                    crate::provider::lookup_path(&src.fields, addr)
                        .map(|v| (addr.clone(), v.clone()))
                })
                .collect()
        })
        .unwrap_or_default();
    let stamps: Vec<(String, PutStamp)> = deeper
        .iter()
        .filter_map(|addr| entry.puts.get(addr).map(|s| (addr.clone(), s.clone())))
        .collect();
    let removed = drop_put_subtree(entry, node, Some(PUT_SOURCE));
    if let Some(src) = entry.sources.get_mut(PUT_SOURCE) {
        for (addr, v) in kept {
            crate::provider::insert_path(&mut src.fields, &addr, v);
        }
        if src.fields.is_empty() {
            entry.sources.remove(PUT_SOURCE);
        }
    }
    entry.puts.extend(stamps);
    removed
}

fn drop_put_subtree_all(entry: &mut CacheEntry, node: &str) -> bool {
    drop_put_subtree(entry, node, None)
}

/// Drop every static map whose label starts with `prefix`. The `is_static_label`
/// half is not redundant: it makes it structurally impossible for a prefix that
/// happens to match a Source's name to take that Source's records with it.
/// Returns whether anything went.
fn drop_labelled(entry: &mut CacheEntry, prefix: &str) -> bool {
    let before = entry.sources.len();
    entry
        .sources
        .retain(|name, _| !(is_static_label(name) && name.starts_with(prefix)));
    entry.sources.len() != before
}

/// Apply one static write to an entry already under a write guard.
fn apply_static_write(
    entry: &mut CacheEntry,
    label: &str,
    node: &str,
    leaves: Vec<(String, Value)>,
    interval_secs: Option<u64>,
    now: Instant,
) {
    let is_put = label == PUT_SOURCE;
    if is_put {
        // A put states the value at its own address: whatever the put map held
        // there, and every address below it, goes first. The label's map is
        // kept only when there is something to fill it with; otherwise it
        // would survive the drop and never be written, leaving a phantom empty
        // static map (and, at the root, an entry the caller's emptiness check
        // would no longer remove).
        drop_put_subtree(entry, node, (!leaves.is_empty()).then_some(label));
    }

    // Rule 1: one record, one map, among `config:` labels only. A later file's
    // record moves out of an earlier file's map. The `put` map neither evicts
    // a `config:` record nor is evicted by one: it is a layer of its own.
    if label.starts_with(CONFIG_SOURCE_PREFIX) {
        for (tail, _) in &leaves {
            let holders: Vec<String> = entry
                .sources
                .iter()
                .filter(|(name, src)| {
                    name.starts_with(CONFIG_SOURCE_PREFIX)
                        && name.as_str() != label
                        && occupies(&src.fields, tail)
                })
                .map(|(name, _)| name.clone())
                .collect();
            for holder in holders {
                if let Some(src) = entry.sources.get_mut(&holder) {
                    remove_occupant(&mut src.fields, tail);
                    if src.fields.is_empty() {
                        entry.sources.remove(&holder);
                    }
                }
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
    if is_put {
        // A scalar standing on the way to `node` is about to become an object:
        // the address that stamped it no longer states that value.
        let mut at = node;
        while let Some(i) = at.rfind('.') {
            at = &at[..i];
            if crate::provider::lookup_path(&src.fields, at)
                .is_some_and(|v| !matches!(v, Value::Object(_)))
            {
                entry.puts.remove(at);
            }
        }
        entry.puts.insert(
            node.to_string(),
            PutStamp {
                written: now,
                interval_secs,
            },
        );
    }
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

/// Merge one source map into a [`MergedRead`], recording each record it lands
/// as `fresh`. `prefix` is the dotted tail of the node `overlay` sits at.
///
/// The overlay is borrowed, not consumed: only the leaves that land are cloned.
/// The chain is walked farthest-first, so a far layer's records are still
/// cloned before a nearer one overwrites them — what the borrow saves is
/// cloning a whole source map to merge a few leaves out of it, which every read
/// used to pay for every contributing map. Objects present on both sides are
/// stitched together exactly as `provider::deep_merge` does; anything else
/// replaces what is there, taking the shadowed records' provenance with it.
fn merge_layer(
    fields: &mut HashMap<String, Value>,
    records: &mut HashMap<String, RecordFreshness>,
    overlay: &HashMap<String, Value>,
    fresh: &RecordFreshness,
    prefix: &str,
) {
    for (k, v) in overlay {
        let tail = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match (fields.get_mut(k), v) {
            (Some(Value::Object(existing)), Value::Object(incoming)) => {
                // An empty object is a record at its own tail, so a farther
                // layer's `{}` leaves provenance standing at a node this layer
                // is about to fill: the records below it are the answer now,
                // and the empty one is not among them.
                if !incoming.is_empty() {
                    records.remove(&tail);
                }
                merge_layer(existing, records, incoming, fresh, &tail);
            }
            (_, v) => {
                // The insert's return value says whether anything was displaced,
                // so the shadowing check costs no extra lookup.
                if fields.insert(k.clone(), v.clone()).is_some() {
                    forget_records(records, &tail);
                }
                note_records(records, &tail, v, fresh);
            }
        }
    }
}

/// Merge one layer's finished read into the chain's: the same per-leaf rules
/// as [`merge_layer`], but each record keeps the freshness the layer gave it.
fn merge_read(
    fields: &mut HashMap<String, Value>,
    records: &mut HashMap<String, RecordFreshness>,
    overlay: &HashMap<String, Value>,
    overlay_records: &HashMap<String, RecordFreshness>,
    prefix: &str,
) {
    for (k, v) in overlay {
        let tail = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match (fields.get_mut(k), v) {
            (Some(Value::Object(existing)), Value::Object(incoming)) => {
                if !incoming.is_empty() {
                    records.remove(&tail);
                }
                merge_read(existing, records, incoming, overlay_records, &tail);
            }
            (_, v) => {
                if fields.insert(k.clone(), v.clone()).is_some() {
                    forget_records(records, &tail);
                }
                copy_records(records, overlay_records, &tail, v);
            }
        }
    }
}

/// Copy the freshness of every leaf of `value`, which sits at `tail`.
fn copy_records(
    records: &mut HashMap<String, RecordFreshness>,
    from: &HashMap<String, RecordFreshness>,
    tail: &str,
    value: &Value,
) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (k, v) in map {
                copy_records(records, from, &format!("{tail}.{k}"), v);
            }
        }
        _ => {
            if let Some(fresh) = from.get(tail) {
                records.insert(tail.to_string(), fresh.clone());
            }
        }
    }
}

/// Record the freshness of every leaf of `value`, which sits at `tail`. A leaf
/// is any non-object plus an empty object, mirroring [`walk_leaves`] — that is
/// what a record is.
fn note_records(
    records: &mut HashMap<String, RecordFreshness>,
    tail: &str,
    value: &Value,
    fresh: &RecordFreshness,
) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (k, v) in map {
                note_records(records, &format!("{tail}.{k}"), v, fresh);
            }
        }
        _ => {
            records.insert(tail.to_string(), fresh.clone());
        }
    }
}

/// Record the freshness of every leaf of a put value at `tail`, from the put
/// address that governs it. A leaf no stamp covers keeps the map's own.
fn note_put_records(
    entry: &CacheEntry,
    label: &Arc<str>,
    records: &mut HashMap<String, RecordFreshness>,
    tail: &str,
    value: &Value,
) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (k, v) in map {
                note_put_records(entry, label, records, &format!("{tail}.{k}"), v);
            }
        }
        _ => {
            if let Some(stamp) = entry.put_stamp_for(tail) {
                records.insert(
                    tail.to_string(),
                    RecordFreshness {
                        source: label.clone(),
                        last_refreshed: stamp.written,
                        expected_interval_secs: stamp.interval_secs,
                    },
                );
            }
        }
    }
}

/// Drop the provenance of the node at `tail` and everything below it — the
/// records a nearer layer has just overwritten.
fn forget_records(records: &mut HashMap<String, RecordFreshness>, tail: &str) {
    records.remove(tail);
    let below = format!("{tail}.");
    records.retain(|k, _| !k.starts_with(&below));
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
    /// Creates the entry if absent; updates only `source_name`'s record if present.
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
                puts: BTreeMap::new(),
                created_at: Instant::now(),
            });
        if let Some(ref watchers) = self.watchers {
            watchers.notify(provider, path);
        }
    }

    /// Write static data at `node`, the dotted tail below `namespace` (`""` for
    /// the namespace root), under the static source label `label`.
    ///
    /// Every leaf of `value` becomes one record. A `put` states the value at
    /// `node`: the subtree there (and every put address below it) is dropped
    /// from the `put` map first, so nothing merges implicitly — nothing else
    /// is removed. A `config:` label adds its leaves to its own map.
    ///
    /// There is no ownership: every supplier writes a layer of its own and a
    /// read stacks them (source, then `config:` seeds, then `put`). Among
    /// `config:` labels a record lives in one map, the latest file's.
    #[allow(clippy::too_many_arguments)]
    pub fn write_static(
        &self,
        namespace: &str,
        path: Option<&str>,
        label: &str,
        node: &str,
        value: Value,
        interval_secs: Option<u64>,
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

        match self.entries.entry(key) {
            dashmap::Entry::Occupied(mut occupied) => {
                apply_static_write(occupied.get_mut(), label, node, leaves, interval_secs, now);
                if occupied.get().sources.is_empty() {
                    occupied.remove();
                }
            }
            dashmap::Entry::Vacant(vacant) => {
                // A write with no leaves (`put ns '{}'`) has nothing to store
                // and, with no entry here yet, nothing to drop either: it must
                // not conjure an entry.
                if leaves.is_empty() {
                    return Ok(());
                }
                let mut fresh = CacheEntry {
                    sources: HashMap::new(),
                    puts: BTreeMap::new(),
                    created_at: now,
                };
                apply_static_write(&mut fresh, label, node, leaves, interval_secs, now);
                vacant.insert(fresh);
            }
        }

        if let Some(ref watchers) = self.watchers {
            watchers.notify(namespace, path);
        }
        Ok(())
    }

    /// Remove the `put` subtree at `node` — the dotted tail below `namespace`,
    /// `""` for the namespace root. The layers below (Sources, `config:` seeds)
    /// are untouched and show through at once. The entry is dropped when no
    /// map remains.
    ///
    /// Returns whether anything was removed.
    pub fn remove_put(&self, namespace: &str, path: Option<&str>, node: &str) -> bool {
        self.remove_put_with(namespace, path, node, drop_put_subtree_all)
    }

    /// Evict one put address on expiry: only the leaves no deeper put
    /// address owns go.
    pub fn evict_put(&self, namespace: &str, path: Option<&str>, node: &str) -> bool {
        self.remove_put_with(namespace, path, node, drop_put_address)
    }

    fn remove_put_with(
        &self,
        namespace: &str,
        path: Option<&str>,
        node: &str,
        drop: fn(&mut CacheEntry, &str) -> bool,
    ) -> bool {
        let key = make_cache_key(namespace, path);

        let mut removed = false;
        if let dashmap::Entry::Occupied(mut occupied) = self.entries.entry(key) {
            removed = drop(occupied.get_mut(), node);
            if occupied.get().sources.is_empty() {
                occupied.remove();
            }
        }

        if removed && let Some(ref watchers) = self.watchers {
            watchers.notify(namespace, path);
        }
        removed
    }

    /// The namespaces at the **global entry** holding a record whose static
    /// source label starts with `prefix` — the namespaces a re-seed has to
    /// visit even when nothing writes to them any more.
    ///
    /// Global-only because the one writer of a `config:` label seeds at the
    /// global path: a facet table in configuration is not addressed to a
    /// directory. A path-scoped entry could not hold one to find.
    pub fn global_namespaces_with_label_prefix(&self, prefix: &str) -> Vec<String> {
        self.entries
            .iter()
            .filter(|entry| {
                entry
                    .value()
                    .sources
                    .keys()
                    .any(|name| is_static_label(name) && name.starts_with(prefix))
            })
            // A key with no null byte in it is the global entry
            // ([`make_cache_key`]); anything else carries a path.
            .filter(|entry| !entry.key().contains('\0'))
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Swap one entry's `prefix`-labelled static records for `writes`, under a
    /// single write guard: every static map whose label starts with `prefix`
    /// goes, then each `(label, value)` is deep-merged at the namespace root in
    /// order, exactly as [`Cache::write_static`] would write it.
    ///
    /// One guard is the point. A drop followed by a separate write leaves a
    /// window in which a concurrent `get` sees neither — a re-seed would blink
    /// every template off and on again. Records a Source produces are never
    /// touched (`prefix` only ever names static labels), and neither is the
    /// `put` map: it sits above the seeds, so configuration seeds a default
    /// and a `put` is the operator saying otherwise.
    ///
    /// An empty `writes` is a removal: the namespace's config records go and
    /// nothing replaces them.
    pub fn reseed_static(
        &self,
        namespace: &str,
        path: Option<&str>,
        drop_label_prefix: &str,
        writes: Vec<(String, Value)>,
    ) -> Result<(), PutError> {
        // Every write is validated and flattened before the guard is taken, so
        // a malformed one refuses without having held a lock.
        let mut planned: Vec<(String, Vec<(String, Value)>)> = Vec::with_capacity(writes.len());
        for (label, value) in writes {
            if !matches!(value, Value::Object(_)) {
                return Err(PutError::RootNotObject);
            }
            if let Some(key) = dotted_key(&value) {
                return Err(PutError::DottedKey {
                    key: key.to_string(),
                });
            }
            let mut leaves = Vec::new();
            walk_leaves("", value, &mut leaves);
            planned.push((label, leaves));
        }
        let key = make_cache_key(namespace, path);
        let now = Instant::now();
        let mut changed = false;
        match self.entries.entry(key) {
            dashmap::Entry::Occupied(mut occupied) => {
                let entry = occupied.get_mut();
                changed |= drop_labelled(entry, drop_label_prefix);
                for (label, leaves) in planned {
                    changed |= !leaves.is_empty();
                    apply_static_write(entry, &label, "", leaves, None, now);
                }
                if occupied.get().sources.is_empty() {
                    occupied.remove();
                }
            }
            dashmap::Entry::Vacant(vacant) => {
                let mut fresh = CacheEntry {
                    sources: HashMap::new(),
                    puts: BTreeMap::new(),
                    created_at: now,
                };
                for (label, leaves) in planned {
                    changed |= !leaves.is_empty();
                    apply_static_write(&mut fresh, &label, "", leaves, None, now);
                }
                if !fresh.sources.is_empty() {
                    vacant.insert(fresh);
                }
            }
        }

        // Notified after the guard is released: waking a watcher while holding
        // a shard guard invites a reader on the same shard to deadlock behind it.
        if changed && let Some(ref watchers) = self.watchers {
            watchers.notify(namespace, path);
        }
        Ok(())
    }

    /// Merge the static maps of `entry` into `out`: `config:<file>` seeds in
    /// label order, then the `put` map on top. Returns whether anything merged.
    ///
    /// One layer at a time: [`Cache::read_merged`] walks the chain itself
    /// because a layer's static maps have to land between its neighbours'
    /// provider maps, not after all of them.
    fn merge_static_records(entry: &CacheEntry, out: &mut MergedRead) -> bool {
        let seeds = Self::merge_named(entry, out, |name| name.starts_with(CONFIG_SOURCE_PREFIX));
        let puts = Self::merge_puts(entry, out);
        seeds | puts
    }

    /// Stack the `put` map on top of everything merged so far. Every put
    /// address first cuts the tree below it, in ascending address order, so a
    /// put replaces the node it was written at instead of merging into it;
    /// then the map lands, each record carrying the freshness of the nearest
    /// stamp at or above it.
    fn merge_puts(entry: &CacheEntry, out: &mut MergedRead) -> bool {
        let Some(src) = entry.sources.get(PUT_SOURCE) else {
            return false;
        };
        for addr in entry.puts.keys() {
            if addr.is_empty() {
                out.fields.clear();
                out.records.clear();
            } else {
                crate::provider::remove_path(&mut out.fields, addr);
                forget_records(&mut out.records, addr);
            }
        }
        let label: Arc<str> = Arc::from(PUT_SOURCE);
        let fallback = RecordFreshness {
            source: label.clone(),
            last_refreshed: src.last_refreshed,
            expected_interval_secs: src.expected_interval_secs,
        };
        merge_layer(
            &mut out.fields,
            &mut out.records,
            &src.fields,
            &fallback,
            "",
        );
        for (k, v) in &src.fields {
            note_put_records(entry, &label, &mut out.records, k, v);
        }
        true
    }

    /// Merge the provider-produced maps of `entry` whose Source the registry
    /// declares with scope `want`, into `out`, in source-name order. Returns
    /// whether anything merged.
    ///
    /// A map whose source the registry does not declare — a source dropped from
    /// config since it last ran — matches no scope and is not read at all,
    /// rather than being read at every scope and merged more than once.
    fn merge_provider_records(
        entry: &CacheEntry,
        namespace: &str,
        want: crate::provider::SourceScope,
        registry: &ProviderRegistry,
        out: &mut MergedRead,
    ) -> bool {
        let declared = registry.provider_sources(namespace);
        Self::merge_named(entry, out, |name| {
            !is_static_label(name)
                && declared
                    .is_some_and(|ss| ss.iter().any(|sm| sm.name == *name && sm.scope == want))
        })
    }

    /// Deep-merge the maps of `entry` whose name `wanted` accepts, in
    /// source-name order. Returns whether anything merged.
    ///
    /// The name order matters because two maps in one entry can in principle
    /// hold the same key — two sentinel Sources, say, whose field names the
    /// registry cannot check for overlap. Iterating `entry.sources` directly
    /// would let a HashMap's order decide the value.
    fn merge_named(
        entry: &CacheEntry,
        out: &mut MergedRead,
        wanted: impl Fn(&String) -> bool,
    ) -> bool {
        let mut names: Vec<&String> = entry.sources.keys().filter(|n| wanted(n)).collect();
        names.sort();
        let mut merged = false;
        for name in names {
            let src = &entry.sources[name];
            let fresh = RecordFreshness {
                source: Arc::from(name.as_str()),
                last_refreshed: src.last_refreshed,
                expected_interval_secs: src.expected_interval_secs,
            };
            merge_layer(&mut out.fields, &mut out.records, &src.fields, &fresh, "");
            merged = true;
        }
        merged
    }

    /// The value tree a read of `namespace` from `requested` sees, plus one
    /// freshness reading per record in it.
    ///
    /// The two paths are the two halves of a [`QueryPlan`]. `requested` is
    /// where the caller is standing ([`QueryPlan::requested_path`]) and fixes
    /// the chain this walks; `provider_entry` is where the namespace's own
    /// records live ([`QueryPlan::effective_path`]) — `requested` narrowed by a
    /// `PathScoped` source's `canonical_path`, so it can sit above `requested`
    /// (a git work-tree root read from a subdirectory). They differ exactly
    /// when a namespace narrows the path, and the chain must still start at
    /// `requested` or a static record written below `provider_entry` — where a
    /// bare `put` lands — would be unreachable.
    ///
    /// Layers merge farthest-first, so the nearest record ends up on top, and
    /// within a layer the order is fixed rather than left to a HashMap:
    ///
    /// - the **global entry** — its `PathScoped` provider maps (present only
    ///   when `provider_entry` is the global entry itself), then its `Global`
    ///   provider maps over them, then its static maps;
    /// - each **ancestor** of `requested`, farthest first — static maps, plus
    ///   the `PathScoped` provider maps of the one that is `provider_entry`;
    /// - the **requested entry** — its `PathScoped` provider maps when it is
    ///   `provider_entry`, then its static maps over them;
    /// - `provider_entry` itself, last, when it is *not* a link of the chain
    ///   — a walker that landed above `$HOME`, where the chain stops; see
    ///   [`crate::query::read_chain`], which builds the walk order.
    ///
    /// A provider record never walks the chain: it is read only from the entry
    /// its Source's declared scope keys. So a provider declaring the same field
    /// in a `Global` and a `PathScoped` source resolves to the path-scoped
    /// record at a path — the path entry is nearer — and to the `Global`
    /// record under `--global`, where both live at the one entry and the
    /// `Global` map is merged second. Which is what `--global` means: the
    /// answer that does not depend on where you are standing.
    ///
    /// `None` when no layer holds anything: a miss.
    ///
    /// [`QueryPlan`]: crate::query::QueryPlan
    /// [`QueryPlan::requested_path`]: crate::query::QueryPlan::requested_path
    /// [`QueryPlan::effective_path`]: crate::query::QueryPlan::effective_path
    pub fn read_merged(
        &self,
        namespace: &str,
        requested: Option<&str>,
        provider_entry: Option<&str>,
        registry: &ProviderRegistry,
        layers: Layers,
    ) -> Option<MergedRead> {
        use crate::provider::SourceScope::{Global, PathScoped};
        let chain = crate::query::read_chain(requested, provider_entry);
        let mut out = MergedRead {
            fields: HashMap::new(),
            records: HashMap::new(),
        };
        let mut found = false;
        for layer in chain.iter().rev() {
            let Some(entry) = self
                .entries
                .get(&make_cache_key(namespace, layer.as_deref()))
            else {
                continue;
            };
            // A put replaces the node it was written at, but only among this
            // layer's own lower maps: the directory chain keeps its per-leaf
            // merge. So a layer holding put addresses is read on its own, cut,
            // and then merged into what the farther layers gave.
            let mut local = MergedRead {
                fields: HashMap::new(),
                records: HashMap::new(),
            };
            let cuts = layers != Layers::Source && !entry.puts.is_empty();
            let target = if !cuts { &mut out } else { &mut local };
            let mut hit = false;
            // At the global link both arms run, PathScoped first.
            if layers != Layers::Put {
                if layer.as_deref() == provider_entry {
                    hit |= Self::merge_provider_records(
                        &entry, namespace, PathScoped, registry, target,
                    );
                }
                if layer.is_none() {
                    hit |=
                        Self::merge_provider_records(&entry, namespace, Global, registry, target);
                }
            }
            if layers != Layers::Source {
                hit |= Self::merge_static_records(&entry, target);
            }
            if hit && cuts {
                merge_read(
                    &mut out.fields,
                    &mut out.records,
                    &local.fields,
                    &local.records,
                    "",
                );
            }
            found |= hit;
        }

        found.then_some(out)
    }

    /// Whether an entry exists for `namespace` anywhere on the chain from
    /// `path` to the global entry.
    ///
    /// An entry existing *is* the answer to "does this namespace hold a
    /// record?", because every write path prunes an entry the moment its last
    /// source map goes ([`Cache::remove_source`], [`Cache::write_static`],
    /// [`Cache::remove_put`]) — there are no empty entries to mistake
    /// for populated ones.
    ///
    /// The cheap half of [`Cache::read_merged`] — no merge, no clone — for the
    /// one question that does not need the tree: whether a namespace exists at
    /// all.
    pub fn has_records_on_chain(&self, namespace: &str, path: Option<&str>) -> bool {
        crate::query::path_chain(path).iter().any(|layer| {
            self.entries
                .contains_key(&make_cache_key(namespace, layer.as_deref()))
        })
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

    /// Whether the map `source` wrote at `(provider, path)` is past its own
    /// interval.
    ///
    /// Staleness is per Source for the same reason coldness is: each map
    /// carries the interval its own Source declared, and a sibling that is
    /// overdue says nothing about this one. False when the entry or the map is
    /// absent — there is nothing stale about a map that was never written.
    pub fn source_is_stale(&self, provider: &str, path: Option<&str>, source: &str) -> bool {
        self.entries
            .get(&make_cache_key(provider, path))
            .and_then(|e| e.sources.get(source).map(|s| s.is_stale()))
            .unwrap_or(false)
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
                // The `put` map has one row per address, aged by that
                // address's own stamp. The root address lists its top-level
                // keys, one row each, as every namespace's records are.
                if source_name == PUT_SOURCE {
                    for (addr, stamp) in &ce.puts {
                        let rows: Vec<(String, &Value)> = if addr.is_empty() {
                            src.fields.iter().map(|(k, v)| (k.clone(), v)).collect()
                        } else {
                            crate::provider::lookup_path(&src.fields, addr)
                                .map(|v| (addr.clone(), v))
                                .into_iter()
                                .collect()
                        };
                        for (field, v) in rows {
                            out.push(CacheRow {
                                provider: provider.clone(),
                                path: path.clone(),
                                source: source_name.clone(),
                                field,
                                value: serde_json::to_value(v).unwrap_or(serde_json::Value::Null),
                                age_ms: stamp.age_ms(),
                                stale: stamp.is_stale(),
                                kind: Some(RowKind::Static),
                                poll_interval_secs: None,
                                poll_count: None,
                                fsevents_reinstate: None,
                                polls_elapsed: None,
                                next_poll_in_secs: None,
                                failure: None,
                            });
                        }
                    }
                    continue;
                }
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
                        poll_count: None,
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
    pub poll_count: Option<u32>,
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
    Lifecycle {
        decay: u8,
        watches_files: bool,
    },
    Once,
    Static,
    Transient,
    /// A row the CLI rendered from a template leaf for the caller's own
    /// directory and environment. Produced by the CLI, never by the daemon:
    /// a template is data the daemon holds at `<p>:template` and never
    /// evaluates (canon `field_resolution.md` invariant 7), so `list_rows`
    /// never emits this kind and nothing carrying it comes off the wire.
    Template,
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

    /// `RowKind::Template` is produced by the CLI's own rendering, never by
    /// the cache (see the doc comment on the variant) — `list_rows` must
    /// never emit one.
    #[test]
    fn list_rows_never_emits_row_kind_template() {
        let cache = Cache::new();
        cache.put_source(
            "myapp",
            None,
            "put",
            make_fields(&[("greeting", "hello")]),
            None,
        );

        let rows = cache.list_rows();
        assert!(
            !rows.iter().any(|r| r.kind == Some(RowKind::Template)),
            "list_rows must never emit RowKind::Template: {rows:?}"
        );
    }

    // ── static labels beside provider sources ────────────────────────────────

    /// A source label is just a `CacheSourceEntry` name, so a `put` record and a
    /// Source's records share one entry. Evicting the Source's map leaves the
    /// static record — and only the static record — behind.
    #[test]
    fn static_record_survives_its_entry_mate_being_evicted() {
        let cache = Cache::new();

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
                None,
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

    /// A Source refreshing under a put changes only its own layer: the merged
    /// read keeps showing the put, and the Source's map shows the new value.
    #[test]
    fn a_source_refresh_does_not_disturb_a_put_over_it() {
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
                "branch",
                Value::String("pinned".into()),
                None,
            )
            .expect("a put over a Source field is accepted");
        cache.put_source(
            "git",
            Some("/repo"),
            "head",
            make_fields(&[("branch", "dev")]),
            None,
        );

        let read = cache
            .read_merged("git", Some("/repo"), Some("/repo"), &registry, Layers::All)
            .expect("both layers");
        assert_eq!(
            read.fields.get("branch"),
            Some(&Value::String("pinned".into()))
        );
        assert_eq!(&*read.records["branch"].source, PUT_SOURCE);
        let head = cache
            .get_source("git", Some("/repo"), "head")
            .expect("the Source's own layer");
        assert_eq!(
            head.fields.get("branch"),
            Some(&Value::String("dev".into()))
        );
    }

    /// Removing a put deletes only the put layer; the Source's value shows
    /// through at once.
    #[test]
    fn removing_a_put_shows_the_layer_below_at_once() {
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
                "branch",
                Value::String("pinned".into()),
                None,
            )
            .unwrap();
        assert!(cache.remove_put("git", Some("/repo"), "branch"));

        let read = cache
            .read_merged("git", Some("/repo"), Some("/repo"), &registry, Layers::All)
            .expect("the Source's layer");
        assert_eq!(
            read.fields.get("branch"),
            Some(&Value::String("main".into()))
        );
    }

    /// A directory-layer fixture: a `git` Source map `head` holding
    /// `tools: {node: 18, go: 1}` at `/repo`, the shape a put replaces into.
    fn cache_with_tools() -> Cache {
        let cache = Cache::new();
        let mut fields = HashMap::new();
        crate::provider::insert_path(&mut fields, "tools.node", Value::Int(18));
        crate::provider::insert_path(&mut fields, "tools.go", Value::Int(1));
        cache.put_source("git", Some("/repo"), "head", fields, None);
        cache
    }

    fn put(cache: &Cache, node: &str, value: Value) {
        cache
            .write_static("git", Some("/repo"), PUT_SOURCE, node, value, None)
            .expect("put accepted");
    }

    fn read_tools(cache: &Cache) -> MergedRead {
        let registry = crate::provider::registry::ProviderRegistry::with_defaults();
        cache
            .read_merged("git", Some("/repo"), Some("/repo"), &registry, Layers::All)
            .expect("a layer holds records")
    }

    fn tools(entries: &[(&str, Value)]) -> Value {
        Value::Object(
            entries
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        )
    }

    /// A put at a leaf replaces that leaf; a put of an object at the node
    /// replaces the whole node, hiding the Source's other members.
    #[test]
    fn a_put_at_a_node_replaces_the_sources_node() {
        let cache = cache_with_tools();
        put(&cache, "tools.node", Value::Int(20));
        assert_eq!(
            read_tools(&cache).fields["tools"],
            tools(&[("node", Value::Int(20)), ("go", Value::Int(1))]),
            "a leaf put replaces that leaf only"
        );

        put(
            &cache,
            "tools",
            tools(&[("node", Value::String("20".into()))]),
        );
        let read = read_tools(&cache);
        assert_eq!(
            read.fields["tools"],
            tools(&[("node", Value::String("20".into()))]),
            "an object put replaces the node: the Source's `go` is hidden"
        );
        assert!(!read.records.contains_key("tools.go"));
    }

    /// A root put states the whole namespace at its directory.
    #[test]
    fn a_root_put_replaces_the_namespace_at_its_directory() {
        let cache = cache_with_tools();
        cache.put_source(
            "git",
            Some("/repo"),
            "refs",
            make_fields(&[("branch", "main")]),
            None,
        );
        put(&cache, "", tools(&[("note", Value::Int(2))]));
        let read = read_tools(&cache);
        assert_eq!(read.fields.len(), 1, "{:?}", read.fields);
        assert_eq!(read.fields["note"], Value::Int(2));
        // The Source maps are still there, only hidden.
        assert!(cache.get_source("git", Some("/repo"), "head").is_some());
    }

    /// A write at N absorbs every put address strictly below N, and a later
    /// put at the same address replaces the earlier one, siblings included.
    #[test]
    fn a_write_absorbs_put_addresses_below_it() {
        let cache = Cache::new();
        put(&cache, "o.a", Value::Int(1));
        put(&cache, "o.b", Value::Int(2));
        let entry = cache.get_entry("git", Some("/repo")).unwrap();
        assert_eq!(entry.puts.keys().collect::<Vec<_>>(), ["o.a", "o.b"]);

        put(&cache, "o", tools(&[("c", Value::Int(3))]));
        let entry = cache.get_entry("git", Some("/repo")).unwrap();
        assert_eq!(entry.puts.keys().collect::<Vec<_>>(), ["o"]);
        let registry = crate::provider::registry::ProviderRegistry::with_defaults();
        let read = cache
            .read_merged("git", Some("/repo"), Some("/repo"), &registry, Layers::All)
            .unwrap();
        assert_eq!(read.fields["o"], tools(&[("c", Value::Int(3))]));
    }

    /// `{"o":{"x":null}}` states all of `o`: it is empty afterwards, and the
    /// earlier sibling `o.y` is gone with the rest of the old `o`.
    #[test]
    fn a_put_states_the_whole_value_at_its_address() {
        let cache = Cache::new();
        put(&cache, "o.y", Value::Int(1));
        put(&cache, "o", tools(&[]));
        let read = read_tools(&cache);
        assert_eq!(read.fields["o"], tools(&[]));
    }

    /// Removing a leaf below an address keeps the address while it holds
    /// anything, and drops it once nothing is left.
    #[test]
    fn null_below_an_address_removes_the_leaf_and_keeps_the_address() {
        let cache = cache_with_tools();
        put(
            &cache,
            "o",
            tools(&[("x", Value::Int(1)), ("y", Value::Int(2))]),
        );
        assert!(cache.remove_put("git", Some("/repo"), "o.x"));
        let entry = cache.get_entry("git", Some("/repo")).unwrap();
        assert_eq!(entry.puts.keys().collect::<Vec<_>>(), ["o"]);
        assert_eq!(
            read_tools(&cache).fields["o"],
            tools(&[("y", Value::Int(2))])
        );

        assert!(cache.remove_put("git", Some("/repo"), "o.y"));
        let entry = cache.get_entry("git", Some("/repo")).unwrap();
        assert!(entry.puts.is_empty(), "an address holding nothing is gone");
        assert!(!read_tools(&cache).fields.contains_key("o"));
    }

    /// Removing a node drops the put subtree there and every address at or
    /// below it, and nothing else.
    #[test]
    fn removing_drops_addresses_at_and_below() {
        let cache = Cache::new();
        put(&cache, "o", tools(&[("x", Value::Int(1))]));
        put(&cache, "o.y", Value::Int(2));
        put(&cache, "p", Value::Int(3));
        assert!(cache.remove_put("git", Some("/repo"), "o"));
        let entry = cache.get_entry("git", Some("/repo")).unwrap();
        assert_eq!(entry.puts.keys().collect::<Vec<_>>(), ["p"]);
        assert!(cache.remove_put("git", Some("/repo"), ""));
        assert!(cache.get_entry("git", Some("/repo")).is_none());
    }

    /// A put never evicts a config seed, and removing the put leaves the seed.
    #[test]
    fn a_put_does_not_evict_a_config_seed() {
        let cache = Cache::new();
        let registry = crate::provider::registry::ProviderRegistry::new();
        let write = |label: &str, v: &str| {
            cache
                .write_static("p", None, label, "x", Value::String(v.into()), None)
                .unwrap();
        };
        write("config:a.toml", "seed");
        write(PUT_SOURCE, "put");
        let read = cache
            .read_merged("p", None, None, &registry, Layers::All)
            .unwrap();
        assert_eq!(read.fields.get("x"), Some(&Value::String("put".into())));
        assert!(cache.remove_put("p", None, "x"));
        let read = cache
            .read_merged("p", None, None, &registry, Layers::All)
            .unwrap();
        assert_eq!(read.fields.get("x"), Some(&Value::String("seed".into())));
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
            puts: BTreeMap::new(),
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

    /// `read_merged` records provenance per *record*, keyed by its dotted tail
    /// — so two sources contributing leaves to one object are told apart, and a
    /// record a nearer layer shadows takes its provenance with it.
    #[tokio::test(start_paused = true)]
    async fn read_merged_keeps_one_freshness_reading_per_record() {
        let cache = Cache::new();
        let registry = crate::provider::registry::ProviderRegistry::new();
        let write = |label: &str, node: &str, value: Value| {
            cache
                .write_static("p", None, label, node, value, None)
                .expect("put accepted");
        };

        write("config:a.toml", "tree.left", Value::Int(1));
        tokio::time::advance(std::time::Duration::from_secs(7)).await;
        write("put", "tree.right", Value::Int(2));

        let read = cache
            .read_merged("p", None, None, &registry, Layers::All)
            .expect("both maps");
        assert_eq!(
            read.records["tree.left"].age_ms(),
            7000,
            "the left leaf is the older map's record"
        );
        assert_eq!(
            read.records["tree.right"].age_ms(),
            0,
            "the right leaf is the newer map's record"
        );
        assert!(
            !read.records.contains_key("tree"),
            "only leaves are records; the object they share is not one"
        );
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
            puts: BTreeMap::new(),
            created_at: Instant::now(),
        };

        let mut expected = HashMap::new();
        crate::provider::insert_path(&mut expected, "tree.left", Value::Int(1));
        crate::provider::insert_path(&mut expected, "tree.right", Value::Int(2));
        assert_eq!(entry.flatten_fields(), expected);
    }
}
