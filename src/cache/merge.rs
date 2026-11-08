//! Pure merge and resolve: the layers of a namespace read as one tree.

use super::*;
use crate::provider::Value;
use crate::provider::registry::ProviderRegistry;
use crate::query::Instance;
use crate::shape::provider::Bound;
use std::collections::HashMap;
use std::sync::Arc;

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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub answered: bool,
}

impl LayerRecord {
    pub(crate) fn new(
        origin: &Origin,
        facet: Option<&str>,
        path: Option<&str>,
        value: &Value,
        age_ms: u128,
    ) -> Self {
        Self {
            label: origin.presented_label(),
            facet: facet.map(str::to_string),
            path: path.map(str::to_string),
            value: serde_json::to_value(value).unwrap_or(serde_json::Value::Null),
            age_ms,
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

impl Cache {
    /// Merge the static maps of `entry` into `out`: configuration seeds in
    /// composition order, so a later file's record lands over an earlier
    /// one's, then the `put` map on top. Returns whether anything merged.
    ///
    /// One layer at a time: [`Cache::read_merged`] walks the chain itself
    /// because a layer's static maps have to land between its neighbours'
    /// provider maps, not after all of them.
    pub(super) fn merge_static_records(
        entry: &CacheEntry,
        at: Option<&str>,
        out: &mut MergedRead,
    ) -> bool {
        let mut seeds = false;
        for (file, src) in &entry.configs {
            if src.fields.is_empty() {
                continue;
            }
            let fresh = RecordFreshness {
                source: Origin::Config(file.clone()),
                entry: at.map(Arc::from),
                last_refreshed: src.last_refreshed,
                expected_interval_secs: src.expected_interval_secs,
            };
            merge_layer(&mut out.fields, &mut out.records, &src.fields, &fresh, "");
            seeds = true;
        }
        let memos = Self::merge_memos(entry, at, out);
        let puts = Self::merge_puts(entry, at, out);
        seeds | memos | puts
    }

    /// Lay the values of a `<p>:memo` entry over everything merged so far,
    /// each at its own address with its own stamp's freshness. A memo of
    /// absence lays nothing. No other entry holds memos.
    fn merge_memos(entry: &CacheEntry, at: Option<&str>, out: &mut MergedRead) -> bool {
        let Some(src) = &entry.memo else {
            return false;
        };
        let at: Option<Arc<str>> = at.map(Arc::from);
        let mut merged = false;
        for (address, stamp) in &entry.memos {
            let Some(value) = crate::provider::lookup_path(&src.fields, address) else {
                continue;
            };
            crate::provider::insert_path(&mut out.fields, address, value.clone());
            // What stood at the address, below it, or in its way above it is
            // not the answer there now.
            forget_records(&mut out.records, address);
            let mut up = address.to_string();
            while let Some(parent) = libbeachcomber::key::parent(&up).filter(|p| !p.is_empty()) {
                out.records.remove(&parent);
                up = parent;
            }
            let fresh = RecordFreshness {
                source: Origin::Memo,
                entry: at.clone(),
                last_refreshed: stamp.written,
                expected_interval_secs: stamp.interval_secs,
            };
            note_records(&mut out.records, address, value, &fresh);
            merged = true;
        }
        merged
    }

    /// Stack the `put` map on top of everything merged so far. Every put
    /// address first cuts the tree below it, in ascending address order, so a
    /// put replaces the node it was written at instead of merging into it;
    /// then the map lands, each record carrying the freshness of the nearest
    /// stamp at or above it.
    fn merge_puts(entry: &CacheEntry, at: Option<&str>, out: &mut MergedRead) -> bool {
        let Some(src) = &entry.put else {
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
        let at: Option<Arc<str>> = at.map(Arc::from);
        let fallback = RecordFreshness {
            source: Origin::Put,
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
            note_put_records(entry, &at, &mut out.records, k, v);
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
            let held = visited
                .iter()
                .any(|i| i.root.as_deref() == at && i.source == *name)
                .then(|| entry.and_then(|e| e.sources.get_key_value(name.as_str())))
                .flatten()
                .filter(|(_, src)| !src.fields.is_empty())
                .map(|(name, src)| {
                    (
                        &src.fields,
                        RecordFreshness {
                            source: Origin::Source(name.clone()),
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
                let record =
                    crate::provider::source_call::source_call(registry, self, p, s, requested);
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
    /// the memo records first (`<namespace>:memo` up the chain from
    /// `requested`, nearest first, `value` null for a memo of absence), then
    /// the template records (`<namespace>:template` the same way), then, for each link of the read's chain
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
        let memo = facet_of(namespace, Facet::Memo);
        for link in crate::query::read_chain(requested, &[]) {
            if let Some(entry) = self.entries.get(&make_cache_key(&memo, link.as_deref())) {
                let values = entry.memo.as_ref().map(|src| &src.fields);
                for (address, stamp) in &entry.memos {
                    let value = values
                        .and_then(|fields| crate::provider::lookup_path(fields, address))
                        .and_then(|v| serde_json::to_value(v).ok())
                        .unwrap_or(serde_json::Value::Null);
                    out.push((
                        address.clone(),
                        LayerRecord {
                            label: Origin::Memo.presented_label(),
                            facet: Some("memo".to_string()),
                            path: link.clone(),
                            value,
                            age_ms: stamp.age_ms(),
                            answered: false,
                        },
                    ));
                }
            }
        }
        let template = facet_of(namespace, Facet::Template);
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
                let Some((name, src)) = entry.sources.get_key_value(instance.source.as_str())
                else {
                    continue;
                };
                let origin = Origin::Source(name.clone());
                for (tail, value) in leaves_of(&src.fields) {
                    out.push((
                        tail,
                        LayerRecord::new(&origin, None, link.as_deref(), &value, src.age_ms()),
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
        if let Some(src) = &entry.put {
            for (tail, value) in leaves_of(&src.fields) {
                let stamp = entry
                    .put_address_for(&tail)
                    .and_then(|address| entry.puts.get(address));
                let age = stamp.map_or(src.age_ms(), |s| s.age_ms());
                out.push((
                    tail,
                    LayerRecord::new(&Origin::Put, facet, link, &value, age),
                ));
            }
        }
        // A later file wins a leaf two files set, so the files are listed
        // last-first: the one that answers, then each it overrode.
        for (file, src) in entry.configs.iter().rev() {
            let origin = Origin::Config(file.clone());
            for (tail, value) in leaves_of(&src.fields) {
                out.push((
                    tail,
                    LayerRecord::new(&origin, facet, link, &value, src.age_ms()),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::cache::test_support::*;

    /// A registry with a provider `demo` of two config Sources, `a` (field `x`)
    /// and `b` (field `y`), and whatever else `extra` configures.
    fn demo_registry(extra: &str) -> crate::provider::registry::ProviderRegistry {
        let config = crate::config::Config::from_toml(&format!(
            "[daemon.providers.demo.sources.a]\ndata = \"{{{{ 1 }}}}\"\nresult.x = \"{{{{ data }}}}\"\n\
             [daemon.providers.demo.sources.b]\ndata = \"{{{{ 2 }}}}\"\nresult.y = \"{{{{ data }}}}\"\n{extra}"
        ));
        crate::provider::registry::ProviderRegistry::with_config(
            &config,
            &std::env::vars().collect(),
        )
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
        assert_eq!(read.records["x"].source, Origin::source("a"));
        assert_eq!(read.records["y"].source, Origin::source("b"));

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
        assert_eq!(read.records["x"].source, Origin::source("a"));
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
        assert_eq!(
            read.records["x"].source,
            Origin::source("a"),
            "a Source's leaf stays its"
        );
        assert_eq!(read.records["y"].source, Origin::source("b"));
        let z = &read.records["z"];
        assert_eq!(z.source, Origin::Computed("result".into()));
        assert!(
            z.source.is_static(),
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
            read.records["x"].source,
            Origin::Computed("result".into()),
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
            assert_eq!(record.source, by_line.records[tail].source, "{tail}");
        }
    }

    #[test]
    fn config_sources_merge_in_name_order_whatever_the_file_order() {
        let config = crate::config::Config::from_toml(
            "[daemon.providers.ord.sources.zeta]\ndata = \"{{ 1 }}\"\nresult.z = \"{{ data }}\"\n\
             [daemon.providers.ord.sources.alpha]\ndata = \"{{ 2 }}\"\nresult.a = \"{{ data }}\"\n",
        );
        let registry = crate::provider::registry::ProviderRegistry::with_config(
            &config,
            &std::env::vars().collect(),
        )
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
            .write_static(
                "demo",
                None,
                &crate::cache::Origin::Put,
                "p",
                Value::Int(7),
                None,
            )
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
        let registry = crate::provider::registry::ProviderRegistry::with_config(
            &config,
            &std::env::vars().collect(),
        )
        .unwrap();
        let cache = Cache::new();
        assert!(
            cache
                .read_merged("solo", None, &[], &registry, Layers::All)
                .is_none()
        );
    }

    /// `read_merged` records provenance per *record*, keyed by its dotted tail
    /// — so two sources contributing leaves to one object are told apart, and a
    /// record a nearer layer shadows takes its provenance with it.
    #[tokio::test(start_paused = true)]
    async fn read_merged_keeps_one_freshness_reading_per_record() {
        let cache = Cache::new();
        let registry = crate::provider::registry::ProviderRegistry::new();
        let write = |origin: Origin, node: &str, value: Value| {
            cache
                .write_static("p", None, &origin, node, value, None)
                .expect("put accepted");
        };

        write(Origin::Config("a.toml".into()), "tree.left", Value::Int(1));
        tokio::time::advance(std::time::Duration::from_secs(7)).await;
        write(Origin::Put, "tree.right", Value::Int(2));

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
}
