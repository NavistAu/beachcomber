use crate::provider::Value;
use crate::provider::registry::ProviderRegistry;
use crate::query::Instance;
use crate::shape::provider::Bound;
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
pub fn is_static_label(source_name: &str) -> bool {
    source_name == PUT_SOURCE
        || source_name.starts_with(CONFIG_SOURCE_PREFIX)
        || source_name.starts_with(COMPUTED_PREFIX)
}

/// Prefix on the label of a leaf a provider's `result` or `return` line
/// computed, which no one Source holds: `computed:result`, `computed:return`.
/// Like `put` and `config:<file>` it has no Source behind it.
pub const COMPUTED_PREFIX: &str = "computed:";

/// A record's writer label as every reader sees it — a `:cache` answer's
/// `source`, a `status` row's SOURCE: `source(<name>)` for a Source,
/// `config(<file>)` for a configuration seed, `computed(result)` /
/// `computed(return)` for a leaf a provider line computed, and `put` as
/// it is. The function form keeps a Source that happens to be named `put` or
/// `template` from reading as one of those words.
///
/// Storage keeps the short labels (`head`, `config:<file>`,
/// `computed:return`): they key records, lifecycle snapshots and failure
/// state, so only what leaves the daemon is presented.
pub fn presented_label(label: &str) -> String {
    if label == PUT_SOURCE {
        label.to_string()
    } else if let Some(file) = label.strip_prefix(CONFIG_SOURCE_PREFIX) {
        format!("config({file})")
    } else if let Some(line) = label.strip_prefix(COMPUTED_PREFIX) {
        format!("computed({line})")
    } else {
        format!("source({label})")
    }
}

/// One record in the stack of layers a read of one leaf consults: its writer's
/// presented label, the facet it sits in (`template` for a template record),
/// the directory it is stored at, its value as written (a template's raw
/// text), and its freshness. `answered` marks the one a read takes. See
/// [`Cache::layer_stacks`] and the `layers` op.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LayerRecord {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facet: Option<String>,
    pub path: Option<String>,
    pub value: serde_json::Value,
    pub age_ms: u128,
    pub stale: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub answered: bool,
}

impl LayerRecord {
    pub(crate) fn new(
        label: &str,
        facet: Option<&str>,
        path: Option<&str>,
        value: &Value,
        age_ms: u128,
        stale: bool,
    ) -> Self {
        Self {
            label: presented_label(label),
            facet: facet.map(str::to_string),
            path: path.map(str::to_string),
            value: serde_json::to_value(value).unwrap_or(serde_json::Value::Null),
            age_ms,
            stale,
            answered: false,
        }
    }
}

/// Every leaf of a map, as `(dotted tail, value)`.
fn leaves_of(fields: &HashMap<String, Value>) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    if !fields.is_empty() {
        walk_leaves("", Value::Object(fields.clone()), &mut out);
    }
    out
}

/// What a Source's records carry into the `<p>:cache` facet beside them: the
/// schedule that keeps them fresh. Built from the Source's metadata
/// ([`Schedule::of`]), so a record and its schedule are written together and
/// removed together.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Schedule {
    /// `(interval_secs, count)` for a Source that polls.
    pub poll: Option<(u64, u32)>,
    /// The watch patterns, then the absolute paths, for a Source that watches.
    pub watch: Vec<String>,
    /// The Source's own decay depth (`ttl(...)` sets 1), stored as `decay`.
    pub decay: Option<u8>,
}

impl Schedule {
    pub fn poll(interval_secs: u64, count: u32) -> Self {
        Self {
            poll: Some((interval_secs, count)),
            watch: Vec::new(),
            decay: None,
        }
    }

    /// The schedule of a Source: its invalidation strategy and, when it sets
    /// one, its own decay depth.
    pub fn of(source: &crate::provider::SourceMetadata) -> Self {
        Self {
            decay: source.decay,
            ..Self::from_strategy(&source.invalidation)
        }
    }

    pub fn from_strategy(strategy: &crate::provider::InvalidationStrategy) -> Self {
        use crate::provider::InvalidationStrategy::*;
        match strategy {
            Poll {
                interval_secs,
                count,
            } => Self::poll(*interval_secs, *count),
            Watch {
                patterns,
                abs_paths,
            } => Self {
                poll: None,
                watch: patterns.iter().chain(abs_paths).cloned().collect(),
                decay: None,
            },
            WatchAndPoll {
                patterns,
                abs_paths,
                interval_secs,
                count,
            } => Self {
                poll: Some((*interval_secs, *count)),
                watch: patterns.iter().chain(abs_paths).cloned().collect(),
                decay: None,
            },
        }
    }

    /// The stored `:cache` keys of one record: `poll`, `watch` and `decay`.
    fn stored_keys(&self) -> HashMap<String, Value> {
        let mut keys = HashMap::new();
        if let Some((interval_secs, count)) = self.poll {
            keys.insert("poll".to_string(), poll_key(interval_secs, count));
        }
        if let Some(decay) = self.decay {
            keys.insert("decay".to_string(), Value::Int(i64::from(decay)));
        }
        if !self.watch.is_empty() {
            keys.insert(
                "watch".to_string(),
                Value::Array(self.watch.iter().cloned().map(Value::String).collect()),
            );
        }
        keys
    }
}

fn poll_key(interval_secs: u64, count: u32) -> Value {
    Value::Object(HashMap::from([
        (
            "interval".to_string(),
            Value::String(format!("{interval_secs}s")),
        ),
        ("count".to_string(), Value::Int(i64::from(count))),
    ]))
}

/// The namespace holding the `:cache` facet of `namespace`.
fn cache_facet_of(namespace: &str) -> String {
    format!("{namespace}:cache")
}

/// Whether `tail` is at or below the `_` prototype node of a `:cache` facet.
fn is_prototype(tail: &str) -> bool {
    tail == "_" || tail.starts_with("_.")
}

/// Why a static write was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PutError {
    /// The root of a namespace is a map of records, not a single value.
    RootNotObject,
}

impl std::fmt::Display for PutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PutError::RootNotObject => write!(f, "put data at the root must be an object"),
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
            Some(interval) => self.last_refreshed.elapsed().as_secs() > interval,
            None => false,
        }
    }
}

/// Which layers of a namespace a read sees. `Put` is what `put` and config
/// seeds stored; `Source` is what the provider's Sources stored. `Cache` reads
/// them all, to answer for them (`p:cache`): it never warms and never attaches
/// a template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layers {
    All,
    Put,
    Source,
    Cache,
}

/// What a provider's `result` line calls in other providers' Sources, by
/// `(provider, source)`: the record, or why it could not be read.
type CalledRecords = HashMap<(String, String), crate::provider::source_call::CallOutcome>;

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
    /// The `config:<file>` labels of this entry in the order their files
    /// compose — `defaults/`, then the main file, then `conf.d/` — which is
    /// the order they are written in. Each file keeps its own records, so two
    /// may hold the same one; a read takes the later file's.
    pub config_order: Vec<String>,
}

impl CacheEntry {
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
    fn config_labels(&self) -> Vec<&String> {
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
    fn put_stamp_for(&self, tail: &str) -> Option<&PutStamp> {
        self.puts.get(self.put_address_for(tail)?)
    }

    /// The put address governing the record at `tail`: at or above it, nearest
    /// first.
    fn put_address_for(&self, tail: &str) -> Option<&str> {
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

/// Every record a value writes, as `(tail below the namespace, leaf value)`.
///
/// A record is a leaf: any non-object, plus an empty object (which stores an
/// empty node rather than nothing). `prefix` is the node the value is written
/// at — empty for the namespace root, where the value is always an object and
/// its members are what get walked.
pub(crate) fn walk_leaves(prefix: &str, value: Value, out: &mut Vec<(String, Value)>) {
    match value {
        Value::Object(map) if !map.is_empty() || prefix.is_empty() => {
            for (k, v) in map {
                let child = libbeachcomber::key::push(prefix, &k);
                walk_leaves(&child, v, out);
            }
        }
        other => out.push((prefix.to_string(), other)),
    }
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
    entry
        .config_order
        .retain(|name| !(is_static_label(name) && name.starts_with(prefix)));
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
        //
        // An array element being written is emptied in place instead:
        // removing it would close the array up and shift the element after it
        // into the address being written.
        let element = !leaves.is_empty()
            && entry
                .sources
                .get(PUT_SOURCE)
                .is_some_and(|src| is_array_element(&src.fields, node));
        if element {
            entry.puts.retain(|addr, _| !is_at_or_below(addr, node));
            if let Some(src) = entry.sources.get_mut(PUT_SOURCE) {
                crate::provider::insert_path(&mut src.fields, node, Value::Object(HashMap::new()));
            }
        } else {
            drop_put_subtree(entry, node, (!leaves.is_empty()).then_some(label));
        }
    }

    // Every `config:` file keeps its own records, a record two files set
    // included: the later file's wins at read time, and the earlier one's is
    // what it overrode. Files are written in composition order, so the order
    // of first writes is that order.
    if leaves.is_empty() {
        return;
    }
    if label.starts_with(CONFIG_SOURCE_PREFIX) && !entry.config_order.iter().any(|l| l == label) {
        entry.config_order.push(label.to_string());
    }
    let src = entry
        .sources
        .entry(label.to_string())
        .or_insert_with(|| CacheSourceEntry {
            fields: HashMap::new(),
            last_refreshed: now,
            // The `put` map's freshness is its address stamps' to own.
            expected_interval_secs: (!is_put).then_some(interval_secs).flatten(),
        });
    if is_put {
        // A value standing in the way of `node` is about to become an object
        // (`provider::insert_path`): the address that stamped it no longer
        // states that value. An array the next segment indexes is written
        // into, not replaced, and keeps its stamp.
        let mut at = node.to_string();
        while let Some(up) = libbeachcomber::key::parent(&at).filter(|up| !up.is_empty()) {
            let below = libbeachcomber::key::last(&at).unwrap_or_default();
            at = up;
            if crate::provider::lookup_path(&src.fields, &at).is_some_and(|v| in_the_way(v, &below))
            {
                entry.puts.remove(&at);
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
    if !is_put && interval_secs.is_some() {
        src.expected_interval_secs = interval_secs;
    }
}

/// Whether `node` names an element inside an array `fields` holds.
fn is_array_element(fields: &HashMap<String, Value>, node: &str) -> bool {
    let Some(up) = libbeachcomber::key::parent(node).filter(|up| !up.is_empty()) else {
        return false;
    };
    let below = libbeachcomber::key::last(node).unwrap_or_default();
    matches!(
        crate::provider::lookup_path(fields, &up),
        Some(Value::Array(items))
            if crate::provider::index_of(&below, items.len()).is_some_and(|i| i < items.len())
    )
}

/// Whether a write to the child `segment` of `value` replaces `value` rather
/// than writing into it: anything but an object, or an array the segment
/// indexes (inside it, or one past its end).
fn in_the_way(value: &Value, segment: &str) -> bool {
    match value {
        Value::Object(_) => false,
        Value::Array(items) => {
            crate::provider::index_of(segment, items.len()).is_none_or(|i| i > items.len())
        }
        _ => true,
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
pub(crate) fn merge_layer(
    fields: &mut HashMap<String, Value>,
    records: &mut HashMap<String, RecordFreshness>,
    overlay: &HashMap<String, Value>,
    fresh: &RecordFreshness,
    prefix: &str,
) {
    for (k, v) in overlay {
        let tail = libbeachcomber::key::push(prefix, k);
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
pub(crate) fn merge_read(
    fields: &mut HashMap<String, Value>,
    records: &mut HashMap<String, RecordFreshness>,
    overlay: &HashMap<String, Value>,
    overlay_records: &HashMap<String, RecordFreshness>,
    prefix: &str,
) {
    for (k, v) in overlay {
        let tail = libbeachcomber::key::push(prefix, k);
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
                copy_records(records, from, &libbeachcomber::key::push(tail, k), v);
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
pub(crate) fn note_records(
    records: &mut HashMap<String, RecordFreshness>,
    tail: &str,
    value: &Value,
    fresh: &RecordFreshness,
) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (k, v) in map {
                note_records(records, &libbeachcomber::key::push(tail, k), v, fresh);
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
    at: &Option<Arc<str>>,
    records: &mut HashMap<String, RecordFreshness>,
    tail: &str,
    value: &Value,
) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (k, v) in map {
                note_put_records(
                    entry,
                    label,
                    at,
                    records,
                    &libbeachcomber::key::push(tail, k),
                    v,
                );
            }
        }
        _ => {
            if let Some(stamp) = entry.put_stamp_for(tail) {
                records.insert(
                    tail.to_string(),
                    RecordFreshness {
                        source: label.clone(),
                        entry: at.clone(),
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
    records.retain(|k, _| !libbeachcomber::key::within(k, tail));
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
            // A record rewritten with the schedule it already carries (a
            // read-always Source, on every read) only has its clock moved.
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
                    },
                );
            }
        }
        self.insert_source_map(
            make_cache_key(provider, path),
            source_name,
            CacheSourceEntry {
                fields,
                last_refreshed: now,
                expected_interval_secs: schedule.poll.map(|(secs, _)| secs),
            },
        );
        if let Some(ref watchers) = self.watchers {
            watchers.notify(provider, path);
        }
    }

    fn insert_source_map(&self, key: CacheKey, source_name: &str, src: CacheSourceEntry) {
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
                config_order: Vec::new(),
            });
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
    /// read stacks them (source, then `config:` seeds, then `put`). Each
    /// `config:` file keeps its own map, written in composition order, and a
    /// read takes a record two files set from the later one.
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
        if label == PUT_SOURCE && !namespace.contains(':') {
            self.write_put_schedule(namespace, path, node, &value, interval_secs);
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
                    config_order: Vec::new(),
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

    /// Write the schedule of a put into `<namespace>:cache` under the `put`
    /// label, ahead of the put itself: a ttl'd put polls once, for its ttl, and
    /// decays in one step. The put replaces whatever stood at `node` and below
    /// it, so their schedules go first. A root put has no address of its own, so
    /// each top-level key carries the schedule.
    fn write_put_schedule(
        &self,
        namespace: &str,
        path: Option<&str>,
        node: &str,
        value: &Value,
        ttl_secs: Option<u64>,
    ) {
        self.drop_put_schedule(namespace, path, node, drop_put_subtree_all);
        let Some(ttl_secs) = ttl_secs else {
            return;
        };
        let addresses: Vec<String> = match (node, value) {
            ("", Value::Object(top)) => top.keys().cloned().collect(),
            _ => vec![node.to_string()],
        };
        let facet = cache_facet_of(namespace);
        for address in addresses.into_iter().filter(|a| !is_prototype(a)) {
            let stored = Value::Object(HashMap::from([
                ("poll".to_string(), poll_key(ttl_secs, 1)),
                ("decay".to_string(), Value::Int(1)),
            ]));
            self.write_static(&facet, path, PUT_SOURCE, &address, stored, None)
                .expect("a stored schedule is a well-formed put");
        }
    }

    /// Remove the stored schedule of the put at `node` from
    /// `<namespace>:cache`, never the `_` prototypes beside it. The root has no
    /// schedule of its own: it is every top-level key's.
    fn drop_put_schedule(
        &self,
        namespace: &str,
        path: Option<&str>,
        node: &str,
        drop: fn(&mut CacheEntry, &str) -> bool,
    ) {
        let key = make_cache_key(&cache_facet_of(namespace), path);
        let dashmap::Entry::Occupied(mut occupied) = self.entries.entry(key) else {
            return;
        };
        let entry = occupied.get_mut();
        let addresses: Vec<String> = if node.is_empty() {
            entry
                .sources
                .get(PUT_SOURCE)
                .map(|src| src.fields.keys().cloned().collect())
                .unwrap_or_default()
        } else {
            vec![node.to_string()]
        };
        for address in addresses.iter().filter(|a| !is_prototype(a)) {
            drop(entry, address);
        }
        if occupied.get().sources.is_empty() {
            occupied.remove();
        }
    }

    fn remove_put_with(
        &self,
        namespace: &str,
        path: Option<&str>,
        node: &str,
        drop: fn(&mut CacheEntry, &str) -> bool,
    ) -> bool {
        if !namespace.contains(':') {
            self.drop_put_schedule(namespace, path, node, drop);
        }
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
    ///
    /// A label whose records here come back exactly as they were keeps its
    /// time: a record's age says when its file last changed it, not when the
    /// daemon last reloaded. Ages are per label map, so an edit to one leaf
    /// of a file restamps that file's other records in this namespace too.
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
                let before: HashMap<String, (HashMap<String, Value>, Instant)> = entry
                    .sources
                    .iter()
                    .filter(|(name, _)| {
                        is_static_label(name) && name.starts_with(drop_label_prefix)
                    })
                    .map(|(name, src)| (name.clone(), (src.fields.clone(), src.last_refreshed)))
                    .collect();
                changed |= drop_labelled(entry, drop_label_prefix);
                for (label, leaves) in planned {
                    changed |= !leaves.is_empty();
                    apply_static_write(entry, &label, "", leaves, None, now);
                }
                for (name, src) in entry.sources.iter_mut() {
                    if let Some((fields, at)) = before.get(name)
                        && *fields == src.fields
                    {
                        src.last_refreshed = *at;
                    }
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
                    config_order: Vec::new(),
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
    /// composition order, so a later file's record lands over an earlier
    /// one's, then the `put` map on top. Returns whether anything merged.
    ///
    /// One layer at a time: [`Cache::read_merged`] walks the chain itself
    /// because a layer's static maps have to land between its neighbours'
    /// provider maps, not after all of them.
    fn merge_static_records(entry: &CacheEntry, at: Option<&str>, out: &mut MergedRead) -> bool {
        let mut seeds = false;
        for name in entry.config_labels() {
            let src = &entry.sources[name];
            if src.fields.is_empty() {
                continue;
            }
            let fresh = RecordFreshness {
                source: Arc::from(name.as_str()),
                entry: at.map(Arc::from),
                last_refreshed: src.last_refreshed,
                expected_interval_secs: src.expected_interval_secs,
            };
            merge_layer(&mut out.fields, &mut out.records, &src.fields, &fresh, "");
            seeds = true;
        }
        let puts = Self::merge_puts(entry, at, out);
        seeds | puts
    }

    /// Stack the `put` map on top of everything merged so far. Every put
    /// address first cuts the tree below it, in ascending address order, so a
    /// put replaces the node it was written at instead of merging into it;
    /// then the map lands, each record carrying the freshness of the nearest
    /// stamp at or above it.
    fn merge_puts(entry: &CacheEntry, at: Option<&str>, out: &mut MergedRead) -> bool {
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
        let at: Option<Arc<str>> = at.map(Arc::from);
        let fallback = RecordFreshness {
            source: label.clone(),
            entry: at.clone(),
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
            note_put_records(entry, &label, &at, &mut out.records, k, v);
        }
        true
    }

    /// Merge the provider's own records at `entry` into `out`: the provider's
    /// `result` over its Sources' maps at this entry
    /// ([`ProviderLines::assemble`]). Returns whether anything merged. `entry`
    /// is `None` for a provider with no Sources, which has no entry of its own
    /// to read.
    ///
    /// Each Source the registry declares, in declaration order, is bound to its
    /// stored map when the read visited its instance at `at`, and to nothing
    /// otherwise. A map whose Source the registry does not declare — a Source
    /// dropped from config since it last ran — is not bound at all, and neither
    /// is a Source's map at a root this read did not visit it at. A line that
    /// fails to render makes this layer a miss; the failure is logged once.
    ///
    /// [`ProviderLines::assemble`]: crate::shape::provider::ProviderLines::assemble
    fn merge_provider_records(
        entry: Option<&CacheEntry>,
        at: Option<&str>,
        namespace: &str,
        registry: &ProviderRegistry,
        visited: &[Instance],
        called: &CalledRecords,
        out: &mut MergedRead,
    ) -> bool {
        let Some(declared) = registry.provider_sources(namespace) else {
            return false;
        };
        let mut bound = Vec::with_capacity(declared.len());
        for sm in declared {
            let name = &sm.name;
            let held = (!is_static_label(name)
                && visited
                    .iter()
                    .any(|i| i.root.as_deref() == at && i.source == *name))
            .then(|| entry.and_then(|e| e.sources.get(name)))
            .flatten()
            .filter(|src| !src.fields.is_empty())
            .map(|src| {
                (
                    &src.fields,
                    RecordFreshness {
                        source: Arc::from(name.as_str()),
                        entry: at.map(Arc::from),
                        last_refreshed: src.last_refreshed,
                        expected_interval_secs: src.expected_interval_secs,
                    },
                )
            });
            bound.push(Bound { name, held });
        }

        let lines = registry.lines();
        let call = |provider: &str, source: &str| {
            called
                .get(&(provider.to_string(), source.to_string()))
                .cloned()
                .unwrap_or(Ok(None))
        };
        match lines.assemble(namespace, at, &bound, &call, out) {
            Ok(merged) => merged,
            Err(message) => {
                lines.note_error(&message);
                false
            }
        }
    }

    /// What the `result` line of `namespace` calls in other providers'
    /// Sources, read for the directory the read stands in: stored records only,
    /// a read on the read path running nothing. Read before a layer's entry is
    /// held, as the read of a Source takes entries itself.
    fn result_calls(
        &self,
        namespace: &str,
        requested: Option<&str>,
        registry: &ProviderRegistry,
    ) -> CalledRecords {
        let lines = registry.lines();
        if !lines.has_calls(namespace) {
            return CalledRecords::new();
        }
        lines
            .result_calls(namespace)
            .iter()
            .map(|(p, s)| {
                let record = crate::provider::source_call::source_call(
                    registry,
                    self,
                    p,
                    s,
                    requested,
                    crate::provider::source_call::Cold::Skip,
                );
                ((p.clone(), s.clone()), record)
            })
            .collect()
    }

    /// The value tree a read of `namespace` from `requested` sees, plus one
    /// freshness reading per record in it.
    ///
    /// `requested` is where the caller is standing
    /// ([`QueryPlan::requested_path`]) and fixes the directory chain this
    /// walks. `visited` are the instances the read visited, as `(source,
    /// root)`: where the namespace's own records live. A Source's records are
    /// read only at its own visited instances, so a record another Source
    /// stored at the same root, or one a nearest Source stored at a root this
    /// read did not visit it at, never shows.
    ///
    /// A root is usually on the chain (a project root above `requested`, or
    /// `None`, the instance at `~`), but one above `$HOME` is not, and is
    /// walked after it — see [`crate::query::read_chain`], which builds the
    /// walk order.
    ///
    /// Layers merge farthest-first, so the nearest record ends up on top, and
    /// within a layer the order is fixed rather than left to a HashMap: the
    /// provider maps first, then the static maps over them. Static maps
    /// (`config:` seeds and `put`) merge at every link of the chain; provider
    /// maps only at a visited instance. A provider record never walks the chain: it
    /// is read only from an instance the read visited, so an instance the read
    /// did not need to ask does not add to the answer.
    ///
    /// `None` when no layer holds anything: a miss.
    ///
    /// [`QueryPlan::requested_path`]: crate::query::QueryPlan::requested_path
    pub fn read_merged(
        &self,
        namespace: &str,
        requested: Option<&str>,
        visited: &[Instance],
        registry: &ProviderRegistry,
        layers: Layers,
    ) -> Option<MergedRead> {
        let chain = crate::query::read_chain(requested, &crate::query::roots_of(visited));
        let mut out = MergedRead {
            fields: HashMap::new(),
            records: HashMap::new(),
        };
        let mut found = false;
        // The calls are made from the directory read, the same at every layer.
        let called = if layers != Layers::Put {
            self.result_calls(namespace, requested, registry)
        } else {
            CalledRecords::new()
        };
        // A provider with no Sources is a provider all the same: its `result`
        // is read once, at the global entry (it has no instances), with
        // `sources` empty. The layers below merge over it.
        if layers != Layers::Put
            && registry
                .provider_sources(namespace)
                .is_some_and(<[_]>::is_empty)
        {
            found |= Self::merge_provider_records(
                None,
                None,
                namespace,
                registry,
                &[],
                &called,
                &mut out,
            );
        }
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
            if layers != Layers::Put && visited.iter().any(|i| i.root == *layer) {
                hit |= Self::merge_provider_records(
                    Some(&entry),
                    layer.as_deref(),
                    namespace,
                    registry,
                    visited,
                    &called,
                    target,
                );
            }
            if layers != Layers::Source {
                hit |= Self::merge_static_records(&entry, layer.as_deref(), target);
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

    /// Every record that holds a leaf of `namespace` for a read from
    /// `requested`, as `(tail, record)`, in the order the read consults them:
    /// the template records first (`<namespace>:template` up the chain from
    /// `requested`, nearest first), then, for each link of the read's chain
    /// nearest first ([`crate::query::read_chain`] over `visited`'s roots),
    /// its `put`, its `config(…)` seeds, and the records each Source in
    /// `visited` stored at that link. At one link the `put` comes before the
    /// seeds and the seeds before the Sources, as a read stacks them.
    ///
    /// Nothing here decides which record answered: a `put` that replaced a
    /// node hides records below it at its own link without being one of
    /// theirs, and a provider line can compute a leaf no record holds. The
    /// `layers` op marks the answer from the read itself.
    pub fn layer_stacks(
        &self,
        namespace: &str,
        requested: Option<&str>,
        visited: &[Instance],
    ) -> Vec<(String, LayerRecord)> {
        let mut out = Vec::new();
        let template = format!("{namespace}:template");
        for link in crate::query::read_chain(requested, &[]) {
            if let Some(entry) = self
                .entries
                .get(&make_cache_key(&template, link.as_deref()))
            {
                Self::static_layers(&entry, link.as_deref(), Some("template"), &mut out);
            }
        }
        for link in crate::query::read_chain(requested, &crate::query::roots_of(visited)) {
            let Some(entry) = self
                .entries
                .get(&make_cache_key(namespace, link.as_deref()))
            else {
                continue;
            };
            Self::static_layers(&entry, link.as_deref(), None, &mut out);
            for instance in visited.iter().filter(|i| i.root == link) {
                let Some(src) = entry.sources.get(&instance.source) else {
                    continue;
                };
                for (tail, value) in leaves_of(&src.fields) {
                    out.push((
                        tail,
                        LayerRecord::new(
                            &instance.source,
                            None,
                            link.as_deref(),
                            &value,
                            src.age_ms(),
                            src.is_stale(),
                        ),
                    ));
                }
            }
        }
        out
    }

    /// One link's static layers, `put` first and then each `config(…)` seed.
    fn static_layers(
        entry: &CacheEntry,
        link: Option<&str>,
        facet: Option<&str>,
        out: &mut Vec<(String, LayerRecord)>,
    ) {
        if let Some(src) = entry.sources.get(PUT_SOURCE) {
            for (tail, value) in leaves_of(&src.fields) {
                let stamp = entry
                    .put_address_for(&tail)
                    .and_then(|address| entry.puts.get(address));
                let (age, stale) = stamp.map_or((src.age_ms(), src.is_stale()), |s| {
                    (s.age_ms(), s.is_stale())
                });
                out.push((
                    tail,
                    LayerRecord::new(PUT_SOURCE, facet, link, &value, age, stale),
                ));
            }
        }
        // A later file wins a leaf two files set, so the files are listed
        // last-first: the one that answers, then each it overrode.
        for name in entry.config_labels().into_iter().rev() {
            let src = &entry.sources[name];
            for (tail, value) in leaves_of(&src.fields) {
                out.push((
                    tail,
                    LayerRecord::new(name, facet, link, &value, src.age_ms(), src.is_stale()),
                ));
            }
        }
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

    /// Whether a put at `provider`'s entry `at` states the record at `tail`: a
    /// put address at or above it hides whatever a Source holds there.
    pub fn put_answers(&self, provider: &str, at: Option<&str>, tail: &str) -> bool {
        self.entries
            .get(&make_cache_key(provider, at))
            .is_some_and(|e| e.put_address_for(tail).is_some())
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

    /// The provider names that hold a `put` anywhere (facets excluded): a
    /// namespace a reload removes from configuration but whose puts remain.
    pub fn names_with_puts(&self) -> std::collections::HashSet<String> {
        self.entries
            .iter()
            .filter(|entry| entry.value().sources.contains_key(PUT_SOURCE))
            .map(|entry| {
                let key = entry.key();
                key.split('\0').next().unwrap_or(key).to_string()
            })
            .filter(|name| !name.contains(':'))
            .collect()
    }

    /// Rewrite what is stored about `source`'s schedule at `path` — the poll
    /// interval its staleness is measured against and the `:cache` keys —
    /// without touching its records: a Source retuned in place. Nothing is
    /// written for a Source with no record there.
    pub fn restamp_schedule(
        &self,
        provider: &str,
        path: Option<&str>,
        source_name: &str,
        schedule: Schedule,
    ) {
        let fields: Vec<String> = {
            let Some(mut entry) = self.entries.get_mut(&make_cache_key(provider, path)) else {
                return;
            };
            let Some(src) = entry.sources.get_mut(source_name) else {
                return;
            };
            src.expected_interval_secs = schedule.poll.map(|(secs, _)| secs);
            src.fields.keys().cloned().collect()
        };
        let facet_key = make_cache_key(&cache_facet_of(provider), path);
        let stored = schedule.stored_keys();
        if stored.is_empty() {
            if let Some(mut entry) = self.entries.get_mut(&facet_key) {
                entry.sources.remove(source_name);
            }
            self.entries
                .remove_if(&facet_key, |_, entry| entry.sources.is_empty());
        } else {
            let per_field = fields
                .into_iter()
                .map(|f| (f, Value::Object(stored.clone())))
                .collect();
            self.insert_source_map(
                facet_key,
                source_name,
                CacheSourceEntry {
                    fields: per_field,
                    last_refreshed: Instant::now(),
                    expected_interval_secs: None,
                },
            );
        }
        if let Some(ref watchers) = self.watchers {
            watchers.notify(provider, path);
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
            // A `:cache` entry holds, beside the `_` prototypes a user wrote,
            // the keys the daemon stored for rows already listed under their
            // own namespace: only the prototypes are rows of their own.
            let is_cache_facet = provider.ends_with(":cache");
            for (source_name, src) in &ce.sources {
                if is_cache_facet && !is_static_label(source_name) {
                    continue;
                }
                // The `put` map has one row per address, aged by that
                // address's own stamp. The root address lists its top-level
                // keys, one row each, as every namespace's records are.
                if source_name == PUT_SOURCE {
                    for (addr, stamp) in &ce.puts {
                        if is_cache_facet && !is_prototype(addr) {
                            continue;
                        }
                        let rows: Vec<(String, &Value)> = if addr.is_empty() {
                            // A top-level key with an address of its own is
                            // that address's row, listed once.
                            src.fields
                                .iter()
                                .map(|(k, v)| (libbeachcomber::key::quote(k).into_owned(), v))
                                .filter(|(k, _)| !ce.puts.contains_key(k))
                                .collect()
                        } else {
                            crate::provider::lookup_path(&src.fields, addr)
                                .map(|v| (addr.clone(), v))
                                .into_iter()
                                .collect()
                        };
                        for (field, v) in rows {
                            out.push(CacheRow {
                                shadowed: false,
                                provider: provider.clone(),
                                path: path.clone(),
                                source: source_name.clone(),
                                field,
                                value: serde_json::to_value(v).unwrap_or(serde_json::Value::Null),
                                age_ms: Some(stamp.age_ms()),
                                stale: stamp.is_stale(),
                                // Only an address written with a ttl is a lifecycle entry; the
                                // server fills its kind in.
                                kind: stamp.interval_secs.is_none().then_some(RowKind::Static),
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
                        shadowed: false,
                        provider: provider.clone(),
                        path: path.clone(),
                        source: source_name.clone(),
                        // The row's address below the namespace, canonical so it
                        // can be typed back as a key.
                        field: libbeachcomber::key::quote(field).into_owned(),
                        value,
                        age_ms: Some(age),
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

/// The `status` row and what it carries: the wire's types.
pub use libbeachcomber::protocol::{CacheRow, FailureSnapshot, RowKind};

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

    /// Return a clone of the full `CacheEntry` for (provider, path), or `None`.
    /// Replaces the old `get()`.
    pub fn get_entry(&self, provider: &str, path: Option<&str>) -> Option<CacheEntry> {
        let key = make_cache_key(provider, path);
        self.entries.get(&key).map(|e| e.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_label_but_put_is_presented_in_function_form() {
        assert_eq!(presented_label("head"), "source(head)");
        assert_eq!(presented_label("put"), "put");
        assert_eq!(
            presented_label("config:/c/defaults/aws.toml"),
            "config(/c/defaults/aws.toml)"
        );
        assert_eq!(presented_label("computed:return"), "computed(return)");
        assert_eq!(presented_label("computed:result"), "computed(result)");
        // A Source named like a layer word stays a Source.
        assert_eq!(presented_label("template"), "source(template)");
    }

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
            crate::cache::Schedule::default(),
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

    /// `RowKind::Rendered` is produced by the CLI's own rendering, never by
    /// the cache (see the doc comment on the variant) — `list_rows` must
    /// never emit one.
    #[test]
    fn list_rows_never_emits_row_kind_rendered() {
        let cache = Cache::new();
        cache.put_source(
            "myapp",
            None,
            "put",
            make_fields(&[("greeting", "hello")]),
            crate::cache::Schedule::default(),
        );

        let rows = cache.list_rows();
        assert!(
            !rows.iter().any(|r| r.kind == Some(RowKind::Rendered)),
            "list_rows must never emit RowKind::Rendered: {rows:?}"
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
            crate::cache::Schedule::default(),
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
            crate::cache::Schedule::default(),
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
            crate::cache::Schedule::default(),
        );

        let read = cache
            .read_merged(
                "git",
                Some("/repo"),
                &[
                    Instance::new("head", Some("/repo")),
                    Instance::new("head", None),
                ],
                &registry,
                Layers::All,
            )
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
            crate::cache::Schedule::default(),
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
            .read_merged(
                "git",
                Some("/repo"),
                &[
                    Instance::new("head", Some("/repo")),
                    Instance::new("head", None),
                ],
                &registry,
                Layers::All,
            )
            .expect("the Source's layer");
        assert_eq!(
            read.fields.get("branch"),
            Some(&Value::String("main".into()))
        );
    }

    /// A registry with a provider `demo` of two config Sources, `a` (field `x`)
    /// and `b` (field `y`), and whatever else `extra` configures.
    fn demo_registry(extra: &str) -> crate::provider::registry::ProviderRegistry {
        let config = crate::config::Config::from_toml(&format!(
            "[daemon.providers.demo.sources.a]\ndata = \"{{{{ 1 }}}}\"\nresult.x = \"{{{{ data }}}}\"\n\
             [daemon.providers.demo.sources.b]\ndata = \"{{{{ 2 }}}}\"\nresult.y = \"{{{{ data }}}}\"\n{extra}"
        ));
        crate::provider::registry::ProviderRegistry::with_config(&config, &Default::default())
            .unwrap_or_else(|e| panic!("loads: {e:?}"))
    }

    fn store_demo(cache: &Cache, source: &str, field: &str, value: i64) {
        cache.put_source(
            "demo",
            None,
            source,
            HashMap::from([(field.to_string(), Value::Int(value))]),
            crate::cache::Schedule::default(),
        );
    }

    fn read_demo(
        cache: &Cache,
        registry: &crate::provider::registry::ProviderRegistry,
        visited: &[&str],
    ) -> Option<MergedRead> {
        let visited: Vec<Instance> = visited.iter().map(|s| Instance::new(s, None)).collect();
        cache.read_merged("demo", None, &visited, registry, Layers::All)
    }

    #[test]
    fn the_default_result_merges_every_visited_source_with_its_own_freshness() {
        let (cache, registry) = (Cache::new(), demo_registry(""));
        store_demo(&cache, "a", "x", 1);
        store_demo(&cache, "b", "y", 2);
        let read = read_demo(&cache, &registry, &["a", "b"]).expect("both Sources");
        assert_eq!(
            read.fields,
            HashMap::from([
                ("x".to_string(), Value::Int(1)),
                ("y".to_string(), Value::Int(2))
            ])
        );
        assert_eq!(&*read.records["x"].source, "a");
        assert_eq!(&*read.records["y"].source, "b");

        let read = read_demo(&cache, &registry, &["a"]).expect("one Source");
        assert!(
            !read.fields.contains_key("y"),
            "a Source the read did not visit is not bound: {:?}",
            read.fields
        );
    }

    #[test]
    fn an_explicit_result_decides_which_source_wins() {
        let registry = demo_registry(
            "[daemon.providers.demo]\nresult = \"{{ merge(sources.b(), sources.a()) }}\"\n\
             [daemon.providers.demo.sources.c]\ndata = \"{{ 3 }}\"\nresult.x = \"{{ data }}\"\n",
        );
        let cache = Cache::new();
        store_demo(&cache, "a", "x", 1);
        store_demo(&cache, "c", "x", 3);
        // The line names only a and b: c is never called.
        let read = read_demo(&cache, &registry, &["a", "c"]).expect("a Source");
        assert_eq!(read.fields["x"], Value::Int(1));
        assert_eq!(&*read.records["x"].source, "a");
    }

    #[test]
    fn a_leaf_a_result_computes_is_labelled_and_as_old_as_its_inputs() {
        let registry = demo_registry(
            "[daemon.providers.demo]\nresult = \"{{ merge(sources) | merge({'z': 9}) }}\"\n",
        );
        let cache = Cache::new();
        store_demo(&cache, "a", "x", 1);
        store_demo(&cache, "b", "y", 2);
        let read = read_demo(&cache, &registry, &["a", "b"]).expect("both Sources");
        assert_eq!(read.fields["z"], Value::Int(9));
        assert_eq!(&*read.records["x"].source, "a", "a Source's leaf stays its");
        assert_eq!(&*read.records["y"].source, "b");
        let z = &read.records["z"];
        assert_eq!(&*z.source, "computed:result");
        assert!(
            is_static_label(&z.source),
            "no Source or schedule behind it, so no decay is attached"
        );
        let oldest = read.records["x"]
            .last_refreshed
            .min(read.records["y"].last_refreshed);
        assert_eq!(z.last_refreshed, oldest, "as old as the oldest input");
    }

    #[test]
    fn two_sources_holding_the_same_value_do_not_pick_one() {
        let registry = demo_registry(
            "[daemon.providers.demo]\nresult = \"{{ merge(sources.a(), sources.b()) }}\"\n",
        );
        let cache = Cache::new();
        store_demo(&cache, "a", "x", 1);
        store_demo(&cache, "b", "x", 1);
        let read = read_demo(&cache, &registry, &["a", "b"]).expect("both Sources");
        assert_eq!(
            &*read.records["x"].source, "computed:result",
            "equal values are no evidence of which Source gave it"
        );
    }

    #[test]
    fn the_native_default_merge_equals_the_rendered_one() {
        let rendered = demo_registry(
            "[daemon.providers.demo]\nresult = \"{{ merge(sources.a(), sources.b()) }}\"\n",
        );
        let native = demo_registry("");
        let cache = Cache::new();
        store_demo(&cache, "a", "x", 1);
        store_demo(&cache, "b", "y", 2);
        let by_line = read_demo(&cache, &rendered, &["a", "b"]).unwrap();
        let by_default = read_demo(&cache, &native, &["a", "b"]).unwrap();
        assert_eq!(by_default.fields, by_line.fields);
        for (tail, record) in &by_default.records {
            assert_eq!(&*record.source, &*by_line.records[tail].source, "{tail}");
        }
    }

    #[test]
    fn config_sources_merge_in_name_order_whatever_the_file_order() {
        let config = crate::config::Config::from_toml(
            "[daemon.providers.ord.sources.zeta]\ndata = \"{{ 1 }}\"\nresult.z = \"{{ data }}\"\n\
             [daemon.providers.ord.sources.alpha]\ndata = \"{{ 2 }}\"\nresult.a = \"{{ data }}\"\n",
        );
        let registry =
            crate::provider::registry::ProviderRegistry::with_config(&config, &Default::default())
                .unwrap();
        let names: Vec<&str> = registry
            .provider_sources("ord")
            .unwrap()
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["alpha", "zeta"],
            "the TOML parser keeps no file order, so declaration order is name order"
        );
    }

    #[test]
    fn a_result_that_fails_to_render_is_a_miss_logged_once() {
        let registry = demo_registry("[daemon.providers.demo]\nresult = \"{{ 1 // 0 }}\"\n");
        let cache = Cache::new();
        store_demo(&cache, "a", "x", 1);
        assert!(read_demo(&cache, &registry, &["a"]).is_none());
        assert!(read_demo(&cache, &registry, &["a"]).is_none());
        let message = registry
            .lines()
            .eval(
                "demo",
                crate::shape::provider::Key::Result,
                vec![("sources", minijinja::Value::from(()))],
            )
            .unwrap_err();
        assert!(
            !registry.lines().note_error(&message),
            "the failure was logged by the first read, and not again"
        );
        // A put beside it is the layer above, and still reads.
        cache
            .write_static("demo", None, PUT_SOURCE, "p", Value::Int(7), None)
            .unwrap();
        let read = read_demo(&cache, &registry, &["a"]).expect("the put");
        assert_eq!(
            read.fields,
            HashMap::from([("p".to_string(), Value::Int(7))])
        );
    }

    #[test]
    fn a_result_that_is_not_a_map_is_a_miss() {
        let registry = demo_registry("[daemon.providers.demo]\nresult = \"{{ 3 }}\"\n");
        let cache = Cache::new();
        store_demo(&cache, "a", "x", 1);
        assert!(read_demo(&cache, &registry, &["a"]).is_none());
    }

    #[test]
    fn a_provider_with_no_sources_answers_nothing() {
        let config =
            crate::config::Config::from_toml("[client.providers.solo]\ntemplate.t = \"x\"\n");
        let registry =
            crate::provider::registry::ProviderRegistry::with_config(&config, &Default::default())
                .unwrap();
        let cache = Cache::new();
        assert!(
            cache
                .read_merged("solo", None, &[], &registry, Layers::All)
                .is_none()
        );
    }

    /// A directory-layer fixture: a `git` Source map `head` holding
    /// `tools: {node: 18, go: 1}` at `/repo`, the shape a put replaces into.
    fn cache_with_tools() -> Cache {
        let cache = Cache::new();
        let mut fields = HashMap::new();
        crate::provider::insert_path(&mut fields, "tools.node", Value::Int(18));
        crate::provider::insert_path(&mut fields, "tools.go", Value::Int(1));
        cache.put_source(
            "git",
            Some("/repo"),
            "head",
            fields,
            crate::cache::Schedule::default(),
        );
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
            .read_merged(
                "git",
                Some("/repo"),
                &[
                    Instance::new("head", Some("/repo")),
                    Instance::new("head", None),
                ],
                &registry,
                Layers::All,
            )
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
            crate::cache::Schedule::default(),
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
            .read_merged(
                "git",
                Some("/repo"),
                &[
                    Instance::new("refs", Some("/repo")),
                    Instance::new("refs", None),
                ],
                &registry,
                Layers::All,
            )
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
            .read_merged("p", None, &[], &registry, Layers::All)
            .unwrap();
        assert_eq!(read.fields.get("x"), Some(&Value::String("put".into())));
        assert!(cache.remove_put("p", None, "x"));
        let read = cache
            .read_merged("p", None, &[], &registry, Layers::All)
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
            .read_merged("p", None, &[], &registry, Layers::All)
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
            config_order: Vec::new(),
        };

        let mut expected = HashMap::new();
        crate::provider::insert_path(&mut expected, "tree.left", Value::Int(1));
        crate::provider::insert_path(&mut expected, "tree.right", Value::Int(2));
        assert_eq!(entry.flatten_fields(), expected);
    }
    fn stored(cache: &Cache, ns: &str, path: Option<&str>) -> Option<CacheEntry> {
        cache.get_entry(&cache_facet_of(ns), path)
    }

    fn json(v: &Value) -> serde_json::Value {
        serde_json::to_value(v).unwrap()
    }

    #[test]
    fn put_source_writes_the_schedule_into_the_cache_facet() {
        let cache = Cache::new();
        let schedule =
            Schedule::from_strategy(&crate::provider::InvalidationStrategy::WatchAndPoll {
                patterns: vec![".git/index".into()],
                abs_paths: vec!["/etc/x".into()],
                interval_secs: 30,
                count: 3,
            });
        cache.put_source(
            "git",
            Some("/repo"),
            "status",
            make_fields(&[("dirty", "false"), ("staged", "0")]),
            schedule,
        );

        let facet =
            stored(&cache, "git", Some("/repo")).expect("the facet entry is at the same path");
        let map = &facet.sources["status"].fields;
        assert_eq!(map.len(), 2, "one object per top-level field");
        assert_eq!(
            json(&map["dirty"]),
            serde_json::json!({
                "poll": {"interval": "30s", "count": 3},
                "watch": [".git/index", "/etc/x"],
            })
        );
        assert!(stored(&cache, "git", None).is_none());
        let stored_keys = cache
            .stored_keys("git", Some("/repo"), "status", "dirty")
            .unwrap();
        assert!(stored_keys.contains_key("poll") && stored_keys.contains_key("watch"));
    }

    #[test]
    fn a_sources_own_decay_is_stored_beside_its_poll() {
        let cache = Cache::new();
        cache.put_source(
            "build",
            None,
            "status",
            make_fields(&[("state", "ok")]),
            Schedule {
                decay: Some(1),
                ..Schedule::poll(30, 1)
            },
        );
        let keys = cache.stored_keys("build", None, "status", "state").unwrap();
        assert_eq!(
            json(&Value::Object(keys)),
            serde_json::json!({"poll": {"interval": "30s", "count": 1}, "decay": 1})
        );
    }

    #[test]
    fn a_source_without_a_schedule_stores_nothing() {
        let cache = Cache::new();
        cache.put_source(
            "p",
            None,
            "s",
            make_fields(&[("a", "1")]),
            Schedule::default(),
        );
        assert!(stored(&cache, "p", None).is_none());
    }

    #[test]
    fn remove_source_removes_the_stored_keys() {
        let cache = Cache::new();
        cache.put_source(
            "p",
            None,
            "one",
            make_fields(&[("a", "1")]),
            Schedule::poll(5, 1),
        );
        cache.put_source(
            "p",
            None,
            "two",
            make_fields(&[("b", "2")]),
            Schedule::poll(9, 1),
        );
        cache.remove_source("p", None, "one");

        let facet = stored(&cache, "p", None).expect("the sibling's keys remain");
        assert!(facet.sources.contains_key("two") && !facet.sources.contains_key("one"));
        cache.remove_source("p", None, "two");
        assert!(stored(&cache, "p", None).is_none(), "the empty entry goes");
        assert!(cache.is_empty());
    }

    #[test]
    fn removing_a_source_leaves_a_decay_prototype_alone() {
        let cache = Cache::new();
        cache
            .write_static("p:cache", None, PUT_SOURCE, "_.decay", Value::Int(2), None)
            .unwrap();
        cache.put_source(
            "p",
            None,
            "s",
            make_fields(&[("a", "1")]),
            Schedule::poll(5, 1),
        );
        cache.remove_source("p", None, "s");
        assert_eq!(cache.resolve_decay("p", None), 2);
    }

    #[test]
    fn a_stored_decay_outside_the_range_is_not_a_prototype() {
        let cache = Cache::new();
        for (n, expected) in [(16, 16), (17, 4), (0, 4)] {
            cache
                .write_static("p:cache", None, PUT_SOURCE, "_.decay", Value::Int(n), None)
                .unwrap();
            assert_eq!(cache.resolve_decay("p", None), expected, "decay {n}");
        }
    }

    #[test]
    fn status_does_not_list_stored_keys() {
        let cache = Cache::new();
        cache.put_source(
            "p",
            None,
            "s",
            make_fields(&[("a", "1")]),
            Schedule::poll(5, 1),
        );
        cache
            .write_static("p", None, PUT_SOURCE, "x", Value::Int(1), Some(60))
            .unwrap();
        cache
            .write_static("p:cache", None, PUT_SOURCE, "_.decay", Value::Int(2), None)
            .unwrap();

        let mut rows: Vec<(String, String, String)> = cache
            .list_rows()
            .into_iter()
            .map(|r| (r.provider, r.source, r.field))
            .collect();
        rows.sort();
        assert_eq!(
            rows,
            vec![
                ("p".to_string(), "put".to_string(), "x".to_string()),
                ("p".to_string(), "s".to_string(), "a".to_string()),
                (
                    "p:cache".to_string(),
                    "put".to_string(),
                    "_.decay".to_string()
                ),
            ],
            "the daemon's stored keys describe rows already listed; a prototype is a row of its own"
        );
    }

    #[test]
    fn a_ttl_put_writes_its_stored_keys() {
        let cache = Cache::new();
        cache
            .write_static(
                "build",
                None,
                PUT_SOURCE,
                "status",
                Value::String("ok".into()),
                Some(300),
            )
            .unwrap();
        let keys = cache
            .stored_keys("build", None, PUT_SOURCE, "status")
            .unwrap();
        assert_eq!(
            json(&Value::Object(keys)),
            serde_json::json!({"poll": {"interval": "300s", "count": 1}, "decay": 1})
        );

        // A leaf below the address takes the address's keys.
        let mut tree = HashMap::new();
        tree.insert("c".to_string(), Value::String("1".into()));
        cache
            .write_static(
                "build",
                None,
                PUT_SOURCE,
                "info",
                Value::Object(tree),
                Some(60),
            )
            .unwrap();
        assert!(
            cache
                .stored_keys("build", None, PUT_SOURCE, "info.c")
                .is_some()
        );
        assert!(
            cache
                .stored_keys("build", None, PUT_SOURCE, "other")
                .is_none()
        );

        // A root put stores its keys per top-level key.
        let mut root = HashMap::new();
        root.insert("k".to_string(), Value::String("v".into()));
        cache
            .write_static("root", None, PUT_SOURCE, "", Value::Object(root), Some(10))
            .unwrap();
        assert!(cache.stored_keys("root", None, PUT_SOURCE, "k").is_some());
    }

    #[test]
    fn removing_a_put_removes_its_stored_keys() {
        let cache = Cache::new();
        cache
            .write_static(
                "build",
                None,
                PUT_SOURCE,
                "status",
                Value::String("ok".into()),
                Some(300),
            )
            .unwrap();
        cache
            .write_static(
                "build:cache",
                None,
                PUT_SOURCE,
                "_.decay",
                Value::Int(2),
                None,
            )
            .unwrap();

        cache.remove_put("build", None, "status");
        assert!(
            cache
                .stored_keys("build", None, PUT_SOURCE, "status")
                .is_none()
        );
        assert_eq!(cache.resolve_decay("build", None), 2, "the prototype stays");

        // Expiry and a re-put without a ttl take them too.
        cache
            .write_static("build", None, PUT_SOURCE, "a", Value::Int(1), Some(5))
            .unwrap();
        cache.evict_put("build", None, "a");
        assert!(cache.stored_keys("build", None, PUT_SOURCE, "a").is_none());
        cache
            .write_static("build", None, PUT_SOURCE, "b", Value::Int(1), Some(5))
            .unwrap();
        cache
            .write_static("build", None, PUT_SOURCE, "b", Value::Int(2), None)
            .unwrap();
        assert!(cache.stored_keys("build", None, PUT_SOURCE, "b").is_none());
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

    /// A `--ttl` put's interval belongs to its own address stamp: a later
    /// no-ttl put at another address does not inherit it, so the entry is not
    /// counted stale on the first address's account.
    #[tokio::test(start_paused = true)]
    async fn a_ttl_put_does_not_make_the_put_map_expire() {
        let cache = Cache::new();
        cache
            .write_static(
                "git",
                Some("/repo"),
                PUT_SOURCE,
                "a",
                Value::Int(1),
                Some(1),
            )
            .unwrap();
        cache
            .write_static("git", Some("/repo"), PUT_SOURCE, "b", Value::Int(2), None)
            .unwrap();
        tokio::time::advance(std::time::Duration::from_secs(5)).await;
        assert!(cache.list_entries().iter().all(|e| !e.stale));
    }
}
