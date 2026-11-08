//! The put store: static writes (`put`, config seeds), their removal and persistence.

use super::*;
use crate::provider::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::time::Instant;

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

/// Drop the subtree at `node` (`""` for everything) from the `put` map,
/// pruning it if it empties, unless `keep` — a write is about to land in it. Every put address at or below `node` goes with it, and so does
/// any address the removal left holding nothing. Every other layer — Source
/// maps and configuration seeds — is left alone, so the layer below a removed put
/// shows through at once. Returns whether anything was removed.
fn drop_put_subtree(entry: &mut CacheEntry, node: &str, keep: bool) -> bool {
    let mut removed = false;
    if let Some(src) = entry.put.as_mut() {
        if node.is_empty() {
            removed |= !src.fields.is_empty();
            src.fields.clear();
        } else {
            removed |= crate::provider::remove_path(&mut src.fields, node).is_some();
        }
        if src.fields.is_empty() && !keep {
            entry.put = None;
        }
    }
    entry.puts.retain(|addr, _| !is_at_or_below(addr, node));
    let held = entry.put.as_ref().map(|src| &src.fields);
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
        return drop_put_subtree(entry, node, false);
    }
    let kept: Vec<(String, Value)> = entry
        .put
        .as_ref()
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
    let removed = drop_put_subtree(entry, node, true);
    if let Some(src) = entry.put.as_mut() {
        for (addr, v) in kept {
            crate::provider::insert_path(&mut src.fields, &addr, v);
        }
        if src.fields.is_empty() {
            entry.put = None;
        }
    }
    entry.puts.extend(stamps);
    removed
}

fn drop_put_subtree_all(entry: &mut CacheEntry, node: &str) -> bool {
    drop_put_subtree(entry, node, false)
}

/// Apply one static write to an entry already under a write guard.
fn apply_static_write(
    entry: &mut CacheEntry,
    origin: &Origin,
    node: &str,
    leaves: Vec<(String, Value)>,
    interval_secs: Option<u64>,
    now: Instant,
) {
    let is_put = *origin == Origin::Put;
    if is_put {
        // A put states the value at its own address: whatever the put map held
        // there, and every address below it, goes first. The put map is
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
                .put
                .as_ref()
                .is_some_and(|src| is_array_element(&src.fields, node));
        if element {
            entry.puts.retain(|addr, _| !is_at_or_below(addr, node));
            if let Some(src) = entry.put.as_mut() {
                crate::provider::insert_path(&mut src.fields, node, Value::Object(HashMap::new()));
            }
        } else {
            drop_put_subtree(entry, node, !leaves.is_empty());
        }
    }

    // Every configuration file keeps its own records, a record two files set
    // included: the later file's wins at read time, and the earlier one's is
    // what it overrode. Files are written in composition order, so the order
    // of first writes ([`CacheEntry::configs`]) is that order.
    if leaves.is_empty() {
        return;
    }
    if is_put {
        // A value standing in the way of `node` is about to become an object
        // (`provider::insert_path`): the address that stamped it no longer
        // states that value. An array the next segment indexes is written
        // into, not replaced, and keeps its stamp.
        let mut at = node.to_string();
        while let Some(up) = libbeachcomber::key::parent(&at).filter(|up| !up.is_empty()) {
            let below = libbeachcomber::key::last(&at).unwrap_or_default();
            at = up;
            let held = entry.put.as_ref().map(|src| &src.fields);
            if held
                .and_then(|fields| crate::provider::lookup_path(fields, &at))
                .is_some_and(|v| in_the_way(v, &below))
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
    let src = entry.map_or_insert(origin, || CacheSourceEntry {
        fields: HashMap::new(),
        last_refreshed: now,
        // The `put` map's freshness is its address stamps' to own.
        expected_interval_secs: (!is_put).then_some(interval_secs).flatten(),
        validity: None,
    });
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

/// Record the freshness of every leaf of a put value at `tail`, from the put
/// address that governs it. A leaf no stamp covers keeps the map's own.
pub(super) fn note_put_records(
    entry: &CacheEntry,
    at: &Option<Arc<str>>,
    records: &mut HashMap<String, RecordFreshness>,
    tail: &str,
    value: &Value,
) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (k, v) in map {
                note_put_records(entry, at, records, &libbeachcomber::key::push(tail, k), v);
            }
        }
        _ => {
            if let Some(stamp) = entry.put_stamp_for(tail) {
                records.insert(
                    tail.to_string(),
                    RecordFreshness {
                        source: Origin::Put,
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
pub(super) fn forget_records(records: &mut HashMap<String, RecordFreshness>, tail: &str) {
    records.retain(|k, _| !libbeachcomber::key::within(k, tail));
}

impl Cache {
    /// Keep persisted puts in `store` from now on.
    pub fn set_put_store(&self, store: crate::persist::PutStore) {
        let _ = self.put_store.set(store);
    }

    /// The persisted puts, when this cache's daemon keeps them.
    pub fn put_store(&self) -> Option<&crate::persist::PutStore> {
        self.put_store.get()
    }

    /// Write static data at `node`, the dotted tail below `namespace` (`""` for
    /// the namespace root), as written by the static writer `origin`.
    ///
    /// Every leaf of `value` becomes one record. A `put` states the value at
    /// `node`: the subtree there (and every put address below it) is dropped
    /// from the `put` map first, so nothing merges implicitly — nothing else
    /// is removed. A configuration file adds its leaves to its own map.
    ///
    /// There is no ownership: every supplier writes a layer of its own and a
    /// read stacks them (source, then configuration seeds, then `put`). Each
    /// configuration file keeps its own map, written in composition order, and a
    /// read takes a record two files set from the later one.
    #[allow(clippy::too_many_arguments)]
    pub fn write_static(
        &self,
        namespace: &str,
        path: Option<&str>,
        origin: &Origin,
        node: &str,
        value: Value,
        interval_secs: Option<u64>,
    ) -> Result<(), PutError> {
        if node.is_empty() && !matches!(value, Value::Object(_)) {
            return Err(PutError::RootNotObject);
        }
        if *origin == Origin::Put && !namespace.contains(':') {
            self.write_put_schedule(namespace, path, node, &value, interval_secs);
        }
        let mut leaves = Vec::new();
        walk_leaves(node, value, &mut leaves);
        let key = make_cache_key(namespace, path);
        let now = Instant::now();

        match self.entries.entry(key) {
            dashmap::Entry::Occupied(mut occupied) => {
                apply_static_write(occupied.get_mut(), origin, node, leaves, interval_secs, now);
                if occupied.get().holds_no_map() {
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
                let mut fresh = CacheEntry::empty(now);
                apply_static_write(&mut fresh, origin, node, leaves, interval_secs, now);
                vacant.insert(fresh);
            }
        }

        if let Some(ref watchers) = self.watchers {
            watchers.notify(namespace, path);
        }
        self.memos_follow_write(namespace, node);
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

    /// Write the schedule of a put into `<namespace>:cache` in its `put`
    /// map, ahead of the put itself: a ttl'd put is a poll of no count (live
    /// for its ttl, never polled) that decays in one step as long. The put replaces whatever stood at `node` and below
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
        let facet = facet_of(namespace, Facet::Cache);
        for address in schedule_addresses(node, value) {
            let stored = Value::Object(HashMap::from([
                ("poll".to_string(), poll_key(ttl_secs, 0)),
                ("decay".to_string(), Value::Int(1)),
            ]));
            self.write_static(&facet, path, &Origin::Put, &address, stored, None)
                .expect("a stored schedule is a well-formed put");
        }
    }

    /// Mark the put just written at `node` (`value`, as written) persisted:
    /// `persist: true` beside its schedule in `<namespace>:cache`, where the
    /// next put to the address, its removal or its expiry drops it with the
    /// schedule.
    pub fn mark_persisted(&self, namespace: &str, path: Option<&str>, node: &str, value: &Value) {
        if namespace.contains(':') {
            return;
        }
        let facet = facet_of(namespace, Facet::Cache);
        for address in schedule_addresses(node, value) {
            let mut keys = self
                .entries
                .get(&make_cache_key(&facet, path))
                .and_then(|entry| {
                    match crate::provider::lookup_path(&entry.put.as_ref()?.fields, &address)? {
                        Value::Object(keys) => Some(keys.clone()),
                        _ => None,
                    }
                })
                .unwrap_or_default();
            keys.insert("persist".to_string(), Value::Bool(true));
            self.write_static(
                &facet,
                path,
                &Origin::Put,
                &address,
                Value::Object(keys),
                None,
            )
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
        let key = make_cache_key(&facet_of(namespace, Facet::Cache), path);
        let dashmap::Entry::Occupied(mut occupied) = self.entries.entry(key) else {
            return;
        };
        let entry = occupied.get_mut();
        let addresses: Vec<String> = if node.is_empty() {
            entry
                .put
                .as_ref()
                .map(|src| src.fields.keys().cloned().collect())
                .unwrap_or_default()
        } else {
            vec![node.to_string()]
        };
        for address in addresses.iter().filter(|a| !is_prototype(a)) {
            drop(entry, address);
        }
        if occupied.get().holds_no_map() {
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
            if occupied.get().holds_no_map() {
                occupied.remove();
            }
        }

        if removed && let Some(ref watchers) = self.watchers {
            watchers.notify(namespace, path);
        }
        if removed {
            self.memos_follow_write(namespace, node);
        }
        removed
    }

    /// The namespaces at the **global entry** holding a configuration seed —
    /// the namespaces a re-seed has to visit even when nothing writes to them
    /// any more.
    ///
    /// Global-only because configuration seeds at the global path: a facet
    /// table in configuration is not addressed to a directory. A path-scoped
    /// entry could not hold one to find.
    pub fn global_namespaces_with_config_seeds(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|entry| !entry.value().configs.is_empty())
            // A key with no null byte in it is the global entry
            // ([`make_cache_key`]); anything else carries a path.
            .filter(|entry| !entry.key().contains('\0'))
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Swap one entry's configuration seeds for `writes`, each a
    /// configuration file's ([`Origin::Config`]), under a single write guard:
    /// every configuration file's map goes, then each `(file, value)` is
    /// deep-merged at the namespace root in order, exactly as
    /// [`Cache::write_static`] would write it.
    ///
    /// One guard is the point. A drop followed by a separate write leaves a
    /// window in which a concurrent `get` sees neither — a re-seed would blink
    /// every template off and on again. Records a Source produces are never
    /// touched, and neither is the `put` map: it sits above the seeds, so
    /// configuration seeds a default and a `put` is the operator saying
    /// otherwise.
    ///
    /// An empty `writes` is a removal: the namespace's config records go and
    /// nothing replaces them.
    ///
    /// A file whose records here come back exactly as they were keeps its
    /// time: a record's age says when its file last changed it, not when the
    /// daemon last reloaded. Ages are per file map, so an edit to one leaf
    /// of a file restamps that file's other records in this namespace too.
    pub fn reseed_static(
        &self,
        namespace: &str,
        path: Option<&str>,
        writes: Vec<(Origin, Value)>,
    ) -> Result<(), PutError> {
        // Every write is validated and flattened before the guard is taken, so
        // a malformed one refuses without having held a lock.
        let mut planned: Vec<(Origin, Vec<(String, Value)>)> = Vec::with_capacity(writes.len());
        for (origin, value) in writes {
            debug_assert!(matches!(origin, Origin::Config(_)), "a reseed writes seeds");
            if !matches!(value, Value::Object(_)) {
                return Err(PutError::RootNotObject);
            }
            let mut leaves = Vec::new();
            walk_leaves("", value, &mut leaves);
            planned.push((origin, leaves));
        }
        let key = make_cache_key(namespace, path);
        let now = Instant::now();
        let mut changed = false;
        // The top-level keys whose records the reseed moved: the memos under
        // them are of what they were.
        let mut moved: Vec<String> = Vec::new();
        match self.entries.entry(key) {
            dashmap::Entry::Occupied(mut occupied) => {
                let entry = occupied.get_mut();
                let before = std::mem::take(&mut entry.configs);
                changed |= !before.is_empty();
                for (origin, leaves) in planned {
                    changed |= !leaves.is_empty();
                    apply_static_write(entry, &origin, "", leaves, None, now);
                }
                let was_of = |file: &Arc<str>| before.iter().find(|(f, _)| f == file);
                for (file, src) in entry.configs.iter_mut() {
                    if let Some((_, was)) = was_of(file)
                        && was.fields == src.fields
                    {
                        src.last_refreshed = was.last_refreshed;
                    }
                }
                fn fields_of<'a>(
                    maps: &'a [(Arc<str>, CacheSourceEntry)],
                    file: &Arc<str>,
                ) -> Option<&'a HashMap<String, Value>> {
                    maps.iter()
                        .find(|(f, _)| f == file)
                        .map(|(_, src)| &src.fields)
                }
                let empty = HashMap::new();
                let files: std::collections::BTreeSet<&Arc<str>> = before
                    .iter()
                    .chain(entry.configs.iter())
                    .map(|(file, _)| file)
                    .collect();
                for file in files {
                    let was = fields_of(&before, file).unwrap_or(&empty);
                    let is = fields_of(&entry.configs, file).unwrap_or(&empty);
                    for k in was.keys().chain(is.keys()) {
                        if was.get(k) != is.get(k) {
                            moved.push(libbeachcomber::key::quote(k).into_owned());
                        }
                    }
                }
                if occupied.get().holds_no_map() {
                    occupied.remove();
                }
            }
            dashmap::Entry::Vacant(vacant) => {
                let mut fresh = CacheEntry::empty(now);
                for (origin, leaves) in planned {
                    changed |= !leaves.is_empty();
                    apply_static_write(&mut fresh, &origin, "", leaves, None, now);
                }
                moved.extend(
                    fresh
                        .configs
                        .iter()
                        .flat_map(|(_, src)| src.fields.keys())
                        .map(|k| libbeachcomber::key::quote(k).into_owned()),
                );
                if !fresh.holds_no_map() {
                    vacant.insert(fresh);
                }
            }
        }

        // Notified after the guard is released: waking a watcher while holding
        // a shard guard invites a reader on the same shard to deadlock behind it.
        if changed && let Some(ref watchers) = self.watchers {
            watchers.notify(namespace, path);
        }
        moved.sort();
        moved.dedup();
        for node in moved {
            self.memos_follow_write(namespace, &node);
        }
        Ok(())
    }

    /// Whether a put at `provider`'s entry `at` states the record at `tail`: a
    /// put address at or above it hides whatever a Source holds there.
    pub fn put_answers(&self, provider: &str, at: Option<&str>, tail: &str) -> bool {
        self.entries
            .get(&make_cache_key(provider, at))
            .is_some_and(|e| e.put_address_for(tail).is_some())
    }

    /// The provider names that hold a `put` anywhere (facets excluded): a
    /// namespace a reload removes from configuration but whose puts remain.
    pub fn names_with_puts(&self) -> std::collections::HashSet<String> {
        self.entries
            .iter()
            .filter(|entry| entry.value().put.is_some())
            .map(|entry| {
                let key = entry.key();
                key.split('\0').next().unwrap_or(key).to_string()
            })
            .filter(|name| !name.contains(':'))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::cache::test_support::*;
    use crate::query::Instance;

    /// A Source refreshing under a put changes only its own layer: the merged
    /// read keeps showing the put, and the Source's map shows the new value.
    #[test]
    fn a_source_refresh_does_not_disturb_a_put_over_it() {
        let cache = Cache::new();
        let registry =
            crate::provider::registry::ProviderRegistry::shipped(&std::env::vars().collect());

        cache.put_source(
            "git",
            Some("/repo"),
            "head",
            make_fields(&[("branch", "main")]),
            &crate::provider::Schedule::default(),
            None,
        );
        cache
            .write_static(
                "git",
                Some("/repo"),
                &crate::cache::Origin::Put,
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
            &crate::provider::Schedule::default(),
            None,
        );

        let read = cache
            .read_merged(
                "git",
                &crate::query::Dir::At("/repo".into()),
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
        assert_eq!(read.records["branch"].source, Origin::Put);
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
        let registry =
            crate::provider::registry::ProviderRegistry::shipped(&std::env::vars().collect());

        cache.put_source(
            "git",
            Some("/repo"),
            "head",
            make_fields(&[("branch", "main")]),
            &crate::provider::Schedule::default(),
            None,
        );
        cache
            .write_static(
                "git",
                Some("/repo"),
                &crate::cache::Origin::Put,
                "branch",
                Value::String("pinned".into()),
                None,
            )
            .unwrap();
        assert!(cache.remove_put("git", Some("/repo"), "branch"));

        let read = cache
            .read_merged(
                "git",
                &crate::query::Dir::At("/repo".into()),
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
            &crate::provider::Schedule::default(),
            None,
        );
        cache
    }

    fn put(cache: &Cache, node: &str, value: Value) {
        cache
            .write_static(
                "git",
                Some("/repo"),
                &crate::cache::Origin::Put,
                node,
                value,
                None,
            )
            .expect("put accepted");
    }

    fn read_tools(cache: &Cache) -> MergedRead {
        let registry =
            crate::provider::registry::ProviderRegistry::shipped(&std::env::vars().collect());
        cache
            .read_merged(
                "git",
                &crate::query::Dir::At("/repo".into()),
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
            &crate::provider::Schedule::default(),
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
        let registry =
            crate::provider::registry::ProviderRegistry::shipped(&std::env::vars().collect());
        let read = cache
            .read_merged(
                "git",
                &crate::query::Dir::At("/repo".into()),
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

    /// An empty object put is a value of its own: it is a record, not a
    /// subtree to walk.
    #[test]
    fn a_put_of_an_empty_object_is_recorded_as_a_leaf() {
        let cache = Cache::new();
        cache
            .write_static(
                "git",
                Some("/repo"),
                &crate::cache::Origin::Put,
                "o",
                tools(&[]),
                Some(30),
            )
            .unwrap();
        let read = read_tools(&cache);
        assert_eq!(read.records["o"].expected_interval_secs, Some(30));
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
        let write = |origin: Origin, v: &str| {
            cache
                .write_static("p", None, &origin, "x", Value::String(v.into()), None)
                .unwrap();
        };
        write(Origin::Config("a.toml".into()), "seed");
        write(Origin::Put, "put");
        let read = cache
            .read_merged("p", &crate::query::Dir::Global, &[], &registry, Layers::All)
            .unwrap();
        assert_eq!(read.fields.get("x"), Some(&Value::String("put".into())));
        assert!(cache.remove_put("p", None, "x"));
        let read = cache
            .read_merged("p", &crate::query::Dir::Global, &[], &registry, Layers::All)
            .unwrap();
        assert_eq!(read.fields.get("x"), Some(&Value::String("seed".into())));
    }

    #[test]
    fn a_ttl_put_writes_its_stored_keys() {
        let cache = Cache::new();
        cache
            .write_static(
                "build",
                None,
                &crate::cache::Origin::Put,
                "status",
                Value::String("ok".into()),
                Some(300),
            )
            .unwrap();
        let keys = cache
            .stored_keys("build", None, &crate::cache::Origin::Put, "status")
            .unwrap();
        assert_eq!(
            json(&Value::Object(keys)),
            serde_json::json!({"poll": {"interval": "300s", "count": 0}, "decay": 1})
        );

        // A leaf below the address takes the address's keys.
        let mut tree = HashMap::new();
        tree.insert("c".to_string(), Value::String("1".into()));
        cache
            .write_static(
                "build",
                None,
                &crate::cache::Origin::Put,
                "info",
                Value::Object(tree),
                Some(60),
            )
            .unwrap();
        assert!(
            cache
                .stored_keys("build", None, &crate::cache::Origin::Put, "info.c")
                .is_some()
        );
        assert!(
            cache
                .stored_keys("build", None, &crate::cache::Origin::Put, "other")
                .is_none()
        );

        // A root put stores its keys per top-level key.
        let mut root = HashMap::new();
        root.insert("k".to_string(), Value::String("v".into()));
        cache
            .write_static(
                "root",
                None,
                &crate::cache::Origin::Put,
                "",
                Value::Object(root),
                Some(10),
            )
            .unwrap();
        assert!(
            cache
                .stored_keys("root", None, &crate::cache::Origin::Put, "k")
                .is_some()
        );
    }

    #[test]
    fn removing_a_put_removes_its_stored_keys() {
        let cache = Cache::new();
        cache
            .write_static(
                "build",
                None,
                &crate::cache::Origin::Put,
                "status",
                Value::String("ok".into()),
                Some(300),
            )
            .unwrap();
        cache
            .write_static(
                "build:cache",
                None,
                &crate::cache::Origin::Put,
                "_.decay",
                Value::Int(2),
                None,
            )
            .unwrap();

        cache.remove_put("build", None, "status");
        assert!(
            cache
                .stored_keys("build", None, &crate::cache::Origin::Put, "status")
                .is_none()
        );
        assert_eq!(
            cache.resolve_decay(
                "build",
                &crate::query::Dir::Global,
                &crate::query::Home::default()
            ),
            2,
            "the prototype stays"
        );

        // Expiry and a re-put without a ttl take them too.
        cache
            .write_static(
                "build",
                None,
                &crate::cache::Origin::Put,
                "a",
                Value::Int(1),
                Some(5),
            )
            .unwrap();
        cache.evict_put("build", None, "a");
        assert!(
            cache
                .stored_keys("build", None, &crate::cache::Origin::Put, "a")
                .is_none()
        );
        cache
            .write_static(
                "build",
                None,
                &crate::cache::Origin::Put,
                "b",
                Value::Int(1),
                Some(5),
            )
            .unwrap();
        cache
            .write_static(
                "build",
                None,
                &crate::cache::Origin::Put,
                "b",
                Value::Int(2),
                None,
            )
            .unwrap();
        assert!(
            cache
                .stored_keys("build", None, &crate::cache::Origin::Put, "b")
                .is_none()
        );
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
                &crate::cache::Origin::Put,
                "a",
                Value::Int(1),
                Some(1),
            )
            .unwrap();
        cache
            .write_static(
                "git",
                Some("/repo"),
                &crate::cache::Origin::Put,
                "b",
                Value::Int(2),
                None,
            )
            .unwrap();
        tokio::time::advance(std::time::Duration::from_secs(5)).await;
        assert!(!cache.get_entry("git", Some("/repo")).unwrap().is_stale());
    }
}
