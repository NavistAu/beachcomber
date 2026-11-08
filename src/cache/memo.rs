//! Memos: a template leaf's stored `| cache()` value, its stamp and its keys.

use super::*;
use crate::provider::Value;
use std::collections::HashMap;
use tokio::time::Instant;

/// What a `memo` op stores for one template leaf.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoWrite {
    /// The leaf's value; `None` for a leaf that rendered nothing.
    pub data: Option<Value>,
    /// The client's hash of the leaf's inputs, 16 lowercase hex digits.
    pub hash: String,
    /// The absolute paths whose change invalidates it.
    pub watch: Vec<String>,
    /// How long it is valid for, in seconds.
    pub ttl_secs: Option<u64>,
}

/// A template leaf's memo as a read finds it ([`Cache::memo_record`]): the
/// nearest one up the chain from the directory read.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoRecord {
    /// The value; `None` for a memo of absence.
    pub data: Option<Value>,
    /// The keys stored for it: `hash` until an invalidation drops it, and
    /// `watch`, `poll` and `decay` as its write gave them.
    pub keys: HashMap<String, Value>,
    pub age_ms: u128,
    pub stale: bool,
    /// The directory it is stored at; `None` for the global entry.
    pub entry: Option<String>,
}

impl MemoRecord {
    /// The stored hash, while an invalidation has not dropped it.
    pub fn hash(&self) -> Option<&str> {
        match self.keys.get("hash") {
            Some(Value::String(hash)) => Some(hash),
            _ => None,
        }
    }

    /// Whether a reader whose hash matches may take it: it has its hash and
    /// is within its lifetime.
    pub fn is_live(&self) -> bool {
        self.hash().is_some() && !self.stale
    }

    /// The memo as a `get`'s memo half carries it: `hash` (while it has
    /// one), `stale`, `age_ms` and `data` (absent for a memo of absence).
    pub fn half(&self) -> serde_json::Value {
        let mut half = serde_json::Map::new();
        if let Some(hash) = self.hash() {
            half.insert("hash".into(), hash.into());
        }
        half.insert("stale".into(), self.stale.into());
        half.insert("age_ms".into(), (self.age_ms as u64).into());
        if let Some(data) = self
            .data
            .as_ref()
            .and_then(|d| serde_json::to_value(d).ok())
        {
            half.insert("data".into(), data);
        }
        half.into()
    }

    /// The memo as `<p>:cache.<field>` answers for it: `source` `memo`, its
    /// `age_ms`, and its stored keys.
    pub fn cache_answer(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut answer = serde_json::Map::new();
        answer.insert("age_ms".into(), (self.age_ms as u64).into());
        answer.insert("source".into(), presented_label(MEMO_SOURCE).into());
        for (key, v) in &self.keys {
            if let Ok(v) = serde_json::to_value(v) {
                answer.insert(key.clone(), v);
            }
        }
        answer
    }
}

impl Cache {
    /// Store `memo` as the memo of `provider`'s template leaf `field` (a
    /// canonical address) for a reader at `path`: its value, when it has one,
    /// in `<provider>:memo`, the address's stamp beside it, and its stored
    /// keys (`hash`, `watch`, and with a ttl `poll` and `decay` 1) in
    /// `<provider>:cache`. Whatever memo stood at that address there goes
    /// first, and so does any at an address above or below it: memos sit at
    /// template leaves, and one leaf's address never holds another's. One
    /// notification, at `path`.
    ///
    /// Invalidates nothing: the value is what the template made of the data
    /// as it is.
    pub fn write_memo(&self, provider: &str, path: Option<&str>, field: &str, memo: MemoWrite) {
        let now = Instant::now();
        let mut keys = HashMap::from([("hash".to_string(), Value::String(memo.hash))]);
        if let Some(ttl_secs) = memo.ttl_secs {
            keys.insert("poll".to_string(), poll_key(ttl_secs, 0));
            keys.insert("decay".to_string(), Value::Int(1));
        }
        if !memo.watch.is_empty() {
            keys.insert(
                "watch".to_string(),
                Value::Array(memo.watch.into_iter().map(Value::String).collect()),
            );
        }
        // Held until the memo is stored, so an eviction at this directory
        // (`evict_memo`) cannot see it empty and unlist it in between.
        let mut listed = self.memo_paths.entry(provider.to_string()).or_default();
        listed.insert(path.map(str::to_string));

        self.drop_memos(provider, path, |address| overlaps(address, field));
        {
            let mut entry = self
                .entries
                .entry(make_cache_key(&memo_facet_of(provider), path))
                .or_insert_with(|| CacheEntry::empty(now));
            entry.memos.insert(
                field.to_string(),
                PutStamp {
                    written: now,
                    interval_secs: memo.ttl_secs,
                },
            );
            if let Some(value) = memo.data {
                let src = entry
                    .sources
                    .entry(MEMO_SOURCE.to_string())
                    .or_insert_with(|| CacheSourceEntry::static_at(now));
                crate::provider::insert_path(&mut src.fields, field, value);
                src.last_refreshed = now;
            }
        }
        {
            let mut entry = self
                .entries
                .entry(make_cache_key(&cache_facet_of(provider), path))
                .or_insert_with(|| CacheEntry::empty(now));
            let src = entry
                .sources
                .entry(MEMO_SOURCE.to_string())
                .or_insert_with(|| CacheSourceEntry::static_at(now));
            crate::provider::insert_path(&mut src.fields, field, Value::Object(keys));
            src.last_refreshed = now;
        }
        drop(listed);
        if let Some(ref watchers) = self.watchers {
            watchers.notify(provider, path);
        }
    }

    /// Evict the memo of `provider`'s template leaf `field` at `path` at the
    /// end of its lifecycle: its value, its stamp and its stored keys go, and
    /// the namespace is notified at `path`; a memo at any other address
    /// stays. A directory left holding no memo of `provider` is no longer
    /// listed for it. Whether there was one.
    pub fn evict_memo(&self, provider: &str, path: Option<&str>, field: &str) -> bool {
        let evicted = {
            // Under the provider's listing, as `write_memo` stores under it.
            let listed = self.memo_paths.entry(provider.to_string());
            let evicted = !self
                .drop_memos(provider, path, |address| address == field)
                .is_empty();
            if let dashmap::Entry::Occupied(mut listed) = listed
                && !self
                    .entries
                    .contains_key(&make_cache_key(&memo_facet_of(provider), path))
            {
                listed.get_mut().remove(&path.map(str::to_string));
                if listed.get().is_empty() {
                    listed.remove();
                }
            }
            evicted
        };
        if evicted && let Some(ref watchers) = self.watchers {
            watchers.notify(provider, path);
        }
        evicted
    }

    /// Remove every memo of `provider` at `path` whose address `which` names
    /// — its stamp, its value and its stored keys — and return their
    /// addresses. Each entry is held on its own; an entry left holding
    /// nothing goes.
    fn drop_memos(
        &self,
        provider: &str,
        path: Option<&str>,
        which: impl Fn(&str) -> bool,
    ) -> Vec<String> {
        let mut gone: Vec<String> = Vec::new();
        if let dashmap::Entry::Occupied(mut occupied) = self
            .entries
            .entry(make_cache_key(&memo_facet_of(provider), path))
        {
            let entry = occupied.get_mut();
            gone = entry.memos.keys().filter(|a| which(a)).cloned().collect();
            for address in &gone {
                entry.memos.remove(address);
            }
            if let Some(src) = entry.sources.get_mut(MEMO_SOURCE) {
                for address in &gone {
                    crate::provider::remove_path(&mut src.fields, address);
                }
                if src.fields.is_empty() {
                    entry.sources.remove(MEMO_SOURCE);
                }
            }
            if entry.sources.is_empty() && entry.memos.is_empty() {
                occupied.remove();
            }
        }
        if gone.is_empty() {
            return gone;
        }
        if let dashmap::Entry::Occupied(mut occupied) = self
            .entries
            .entry(make_cache_key(&cache_facet_of(provider), path))
        {
            let entry = occupied.get_mut();
            if let Some(src) = entry.sources.get_mut(MEMO_SOURCE) {
                for address in &gone {
                    crate::provider::remove_path(&mut src.fields, address);
                }
                if src.fields.is_empty() {
                    entry.sources.remove(MEMO_SOURCE);
                }
            }
            if entry.sources.is_empty() {
                occupied.remove();
            }
        }
        gone
    }

    /// Whether `provider` has ever been given a memo: when not, a read has
    /// no memo half to look for.
    pub fn has_memos(&self, provider: &str) -> bool {
        self.memo_paths.contains_key(provider)
    }

    /// The memo of `provider`'s template leaf `tail` (a canonical address) a
    /// read from `chain_from` finds: up the directory chain, nearest first,
    /// the first directory holding a memo record at `tail` deciding. `None`
    /// when there is none — and at once for a provider never given a memo.
    pub fn memo_record(
        &self,
        provider: &str,
        chain_from: Option<&str>,
        tail: &str,
    ) -> Option<MemoRecord> {
        if !self.has_memos(provider) {
            return None;
        }
        let memo_facet = memo_facet_of(provider);
        for link in crate::query::path_chain(chain_from) {
            let (stamp, data) = {
                let Some(entry) = self
                    .entries
                    .get(&make_cache_key(&memo_facet, link.as_deref()))
                else {
                    continue;
                };
                let Some(stamp) = entry.memos.get(tail) else {
                    continue;
                };
                let data = entry
                    .sources
                    .get(MEMO_SOURCE)
                    .and_then(|src| crate::provider::lookup_path(&src.fields, tail))
                    .cloned();
                (stamp.clone(), data)
            };
            let keys = self
                .entries
                .get(&make_cache_key(&cache_facet_of(provider), link.as_deref()))
                .and_then(|entry| {
                    match crate::provider::lookup_path(
                        &entry.sources.get(MEMO_SOURCE)?.fields,
                        tail,
                    )? {
                        Value::Object(keys) => Some(keys.clone()),
                        _ => None,
                    }
                })
                .unwrap_or_default();
            return Some(MemoRecord {
                data,
                keys,
                age_ms: stamp.age_ms(),
                stale: stamp.is_stale(),
                entry: link,
            });
        }
        None
    }

    /// Drop the hash of `provider`'s memo at `field` at `path`, keeping the
    /// rest of its record and its value: a reader then misses it, and the
    /// next render writes a new one. Notifies the namespace at `path` when a
    /// hash went. Whether one did.
    pub fn drop_memo_hash(&self, provider: &str, path: Option<&str>, field: &str) -> bool {
        let dropped = self
            .entries
            .get_mut(&make_cache_key(&cache_facet_of(provider), path))
            .is_some_and(|mut entry| {
                let Some(src) = entry.sources.get_mut(MEMO_SOURCE) else {
                    return false;
                };
                match crate::provider::lookup_path_mut(&mut src.fields, field) {
                    Some(Value::Object(keys)) => keys.remove("hash").is_some(),
                    _ => false,
                }
            });
        if dropped && let Some(ref watchers) = self.watchers {
            watchers.notify(provider, path);
        }
        dropped
    }

    /// Drop the hash of every memo of `provider` whose address overlaps
    /// `node` (`""`: every one), at every directory: an invalidation of the
    /// field, which every reader below any of them shares. Whether any such
    /// memo exists, hash or not.
    pub fn drop_memo_hashes_under(&self, provider: &str, node: &str) -> bool {
        let Some(paths) = self
            .memo_paths
            .get(provider)
            .map(|paths| paths.iter().cloned().collect::<Vec<_>>())
        else {
            return false;
        };
        let memo_facet = memo_facet_of(provider);
        let mut found = false;
        for path in paths {
            let addresses: Vec<String> = match self
                .entries
                .get(&make_cache_key(&memo_facet, path.as_deref()))
            {
                Some(entry) => entry
                    .memos
                    .keys()
                    .filter(|a| overlaps(a, node))
                    .cloned()
                    .collect(),
                None => continue,
            };
            found |= !addresses.is_empty();
            for address in addresses {
                self.drop_memo_hash(provider, path.as_deref(), &address);
            }
        }
        found
    }

    /// A write at `node` of `namespace` (a provider's data, or its
    /// `:template`) invalidates the memos whose leaves it overlaps: their
    /// hashes go. A write to any other facet moves no memo.
    pub(super) fn memos_follow_write(&self, namespace: &str, node: &str) {
        use libbeachcomber::key::Facet;
        if let Ok((base, None | Some(Facet::Template))) =
            libbeachcomber::key::split_facet(namespace)
        {
            self.drop_memo_hashes_under(base, node);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::cache::test_support::*;
    use crate::watcher_registry::WatcherRegistry;
    use std::sync::Arc;

    fn memo(hash: &str, data: Option<Value>, ttl_secs: Option<u64>) -> MemoWrite {
        MemoWrite {
            data,
            hash: hash.to_string(),
            watch: Vec::new(),
            ttl_secs,
        }
    }

    const H1: &str = "9c6001d328a94690";

    const H2: &str = "0123456789abcdef";

    #[test]
    fn a_memo_is_a_value_a_stamp_and_its_keys() {
        let cache = Cache::new();
        cache.write_memo(
            "k",
            None,
            "context",
            MemoWrite {
                watch: vec!["/h/.kube/config".into()],
                ..memo(H1, Some(Value::String("prod".into())), Some(600))
            },
        );
        let record = cache.memo_record("k", None, "context").expect("a record");
        assert_eq!(record.data, Some(Value::String("prod".into())));
        assert_eq!(record.hash(), Some(H1));
        assert!(record.is_live());
        assert_eq!(record.entry, None);
        assert_eq!(
            record.keys["poll"],
            poll_key(600, 0),
            "a ttl is a poll of count 0"
        );
        assert_eq!(record.keys["decay"], Value::Int(1));
        assert_eq!(
            record.keys["watch"],
            Value::Array(vec![Value::String("/h/.kube/config".into())])
        );
        assert!(cache.memo_record("k", None, "namespace").is_none());
        assert!(cache.memo_record("other", None, "context").is_none());
    }

    #[test]
    fn a_memo_of_absence_is_a_record_with_no_value() {
        let cache = Cache::new();
        cache.write_memo("k", None, "context", memo(H1, None, None));
        let record = cache.memo_record("k", None, "context").expect("a record");
        assert_eq!(record.data, None);
        assert_eq!(record.hash(), Some(H1));
        assert_eq!(
            record.half(),
            serde_json::json!({"hash": H1, "stale": false, "age_ms": record.age_ms as u64}),
            "no data, never null"
        );
        // Nothing in `<p>:memo` holds a value for it.
        let entry = cache.get_entry("k:memo", None).expect("the stamp's entry");
        assert!(entry.sources.is_empty());
        assert!(entry.memos.contains_key("context"));
    }

    #[test]
    fn a_memo_is_found_up_the_chain_nearest_first() {
        let cache = Cache::new();
        cache.write_memo("k", Some("/m/a"), "f", memo(H1, Some(Value::Int(1)), None));
        let record = cache.memo_record("k", Some("/m/a/b"), "f").unwrap();
        assert_eq!(
            (record.entry.as_deref(), record.data),
            (Some("/m/a"), Some(Value::Int(1)))
        );
        assert!(cache.memo_record("k", Some("/m"), "f").is_none());
        cache.write_memo(
            "k",
            Some("/m/a/b"),
            "f",
            memo(H2, Some(Value::Int(2)), None),
        );
        let record = cache.memo_record("k", Some("/m/a/b"), "f").unwrap();
        assert_eq!(
            (record.hash(), record.data.clone()),
            (Some(H2), Some(Value::Int(2)))
        );
    }

    #[test]
    fn a_memo_write_replaces_every_memo_it_overlaps_there() {
        let cache = Cache::new();
        cache.write_memo("k", None, "g.x", memo(H1, Some(Value::Int(1)), Some(5)));
        cache.write_memo("k", None, "g.y", memo(H1, Some(Value::Int(2)), None));
        cache.write_memo("k", None, "h", memo(H1, Some(Value::Int(3)), None));
        cache.write_memo("k", None, "g", memo(H2, None, None));
        assert!(cache.memo_record("k", None, "g.x").is_none());
        assert!(cache.memo_record("k", None, "g.y").is_none());
        let g = cache.memo_record("k", None, "g").unwrap();
        assert_eq!((g.hash(), g.data.clone()), (Some(H2), None));
        assert!(
            !g.keys.contains_key("poll"),
            "the old memo's keys went with it"
        );
        assert_eq!(
            cache.memo_record("k", None, "h").unwrap().data,
            Some(Value::Int(3)),
            "a sibling stays"
        );
        // And the other way: one below replaces the one above.
        cache.write_memo("k", None, "g.z", memo(H1, Some(Value::Int(4)), None));
        assert!(cache.memo_record("k", None, "g").is_none());
        assert_eq!(
            cache.memo_record("k", None, "g.z").unwrap().data,
            Some(Value::Int(4))
        );
    }

    /// An evicted memo takes its value, its stamp and its keys and wakes the
    /// namespace there; a memo at another address, or the same address at
    /// another directory, stays. The last memo of a directory unlists it, and
    /// the provider's last its listing.
    #[test]
    fn evicting_a_memo_removes_that_address_only() {
        let watchers = Arc::new(WatcherRegistry::new());
        let cache = Cache::with_watchers(watchers.clone());
        cache.write_memo("k", None, "f", memo(H1, Some(Value::Int(1)), Some(60)));
        cache.write_memo("k", None, "g", memo(H1, None, None));
        cache.write_memo("k", Some("/m"), "f", memo(H2, Some(Value::Int(2)), None));
        let mut woken = watchers.subscribe("k", None);

        assert!(cache.evict_memo("k", None, "f"));
        assert!(woken.try_recv().is_ok(), "the namespace was notified");
        assert!(!cache.evict_memo("k", None, "f"), "only once");
        assert!(woken.try_recv().is_err(), "nothing went the second time");
        assert!(cache.memo_record("k", None, "f").is_none());
        assert!(cache.stored_keys("k", None, MEMO_SOURCE, "f").is_none());
        assert!(cache.stored_keys("k", None, MEMO_SOURCE, "g").is_some());
        assert!(cache.memo_record("k", None, "g").is_some());
        assert_eq!(
            cache.memo_record("k", Some("/m"), "f").unwrap().data,
            Some(Value::Int(2))
        );

        cache.evict_memo("k", None, "g");
        assert!(cache.get_entry("k:memo", None).is_none());
        assert!(cache.get_entry("k:cache", None).is_none());
        assert!(cache.has_memos("k"), "/m holds one still");
        cache.evict_memo("k", Some("/m"), "f");
        assert!(!cache.has_memos("k"), "none left anywhere");
    }

    #[test]
    fn a_dropped_hash_leaves_the_record_and_its_value() {
        let cache = Cache::new();
        cache.write_memo("k", None, "f", memo(H1, Some(Value::Int(1)), Some(60)));
        assert!(cache.drop_memo_hash("k", None, "f"));
        assert!(!cache.drop_memo_hash("k", None, "f"), "only once");
        let record = cache.memo_record("k", None, "f").unwrap();
        assert_eq!(record.hash(), None);
        assert!(!record.is_live());
        assert_eq!(record.data, Some(Value::Int(1)));
        assert_eq!(record.keys["poll"], poll_key(60, 0));
    }

    #[test]
    fn a_write_overlapping_a_memo_drops_its_hash_at_every_directory() {
        let cache = Cache::new();
        let live = |path: Option<&str>, f: &str| {
            cache
                .memo_record("k", path, f)
                .is_some_and(|r| r.hash().is_some())
        };
        let write_all = || {
            for (path, f) in [(None, "f"), (Some("/m/a"), "f"), (None, "g")] {
                cache.write_memo("k", path, f, memo(H1, Some(Value::Int(1)), None));
            }
        };

        write_all();
        cache
            .write_static(
                "k",
                Some("/elsewhere"),
                PUT_SOURCE,
                "f.below",
                Value::Int(1),
                None,
            )
            .unwrap();
        assert!(!live(None, "f") && !live(Some("/m/a"), "f"), "data under f");
        assert!(live(None, "g"), "a sibling stays");

        write_all();
        cache
            .write_static(
                "k:template",
                None,
                PUT_SOURCE,
                "",
                Value::Object(HashMap::from([(
                    "g".to_string(),
                    Value::String("{{ 1 }}".into()),
                )])),
                None,
            )
            .unwrap();
        assert!(
            !live(None, "f") && !live(None, "g"),
            "a template write at the root"
        );

        write_all();
        cache.remove_put("k", Some("/elsewhere"), "f.below");
        assert!(!live(None, "f"), "a removal moves data too");
        assert!(live(None, "g"));

        write_all();
        cache.put_source(
            "k",
            Some("/m/a"),
            "s",
            make_fields(&[("g", "v")]),
            Schedule::default(),
        );
        assert!(!live(None, "g"), "a Source's run");
        assert!(live(None, "f"));

        write_all();
        cache
            .write_static(
                "k:macro",
                None,
                PUT_SOURCE,
                "lib",
                Value::String("".into()),
                None,
            )
            .unwrap();
        cache
            .write_static("k:cache", None, PUT_SOURCE, "_.decay", Value::Int(2), None)
            .unwrap();
        assert!(
            live(None, "f") && live(None, "g"),
            "no other facet moves a memo"
        );

        write_all();
        cache
            .reseed_static(
                "k:template",
                None,
                CONFIG_SOURCE_PREFIX,
                vec![(
                    "config:/c.toml".into(),
                    Value::Object(HashMap::from([(
                        "f".to_string(),
                        Value::String("{{ 2 }}".into()),
                    )])),
                )],
            )
            .unwrap();
        assert!(!live(None, "f"), "a reseed that changed f");
        assert!(live(None, "g"), "and only f");
        write_all();
        cache
            .reseed_static(
                "k:template",
                None,
                CONFIG_SOURCE_PREFIX,
                vec![(
                    "config:/c.toml".into(),
                    Value::Object(HashMap::from([(
                        "f".to_string(),
                        Value::String("{{ 2 }}".into()),
                    )])),
                )],
            )
            .unwrap();
        assert!(
            live(None, "f"),
            "a reseed that changed nothing moves nothing"
        );
    }

    #[test]
    fn a_read_of_the_memo_facet_ages_each_memo_by_its_own_stamp() {
        let cache = Cache::new();
        let registry = crate::provider::registry::ProviderRegistry::new();
        cache.write_memo("k", None, "f", memo(H1, Some(Value::Int(1)), Some(30)));
        cache.write_memo("k", None, "g", memo(H1, Some(Value::Int(2)), None));
        cache.write_memo("k", None, "none", memo(H1, None, None));
        let read = cache
            .read_merged("k:memo", None, &[], &registry, Layers::All)
            .expect("a hit");
        assert_eq!(
            read.fields,
            HashMap::from([
                ("f".to_string(), Value::Int(1)),
                ("g".to_string(), Value::Int(2)),
            ]),
            "a memo of absence holds no value"
        );
        assert_eq!(read.records["f"].expected_interval_secs, Some(30));
        assert_eq!(read.records["g"].expected_interval_secs, None);
        assert_eq!(&*read.records["f"].source, MEMO_SOURCE);
        assert_eq!(presented_label(MEMO_SOURCE), "memo");
        assert!(is_static_label(MEMO_SOURCE));
    }

    #[test]
    fn a_memo_with_a_value_is_a_row_of_its_facet_and_its_keys_are_none() {
        let cache = Cache::new();
        cache.write_memo(
            "k",
            Some("/m"),
            "f",
            memo(H1, Some(Value::Int(1)), Some(30)),
        );
        cache.write_memo("k", Some("/m"), "none", memo(H1, None, None));
        let rows = cache.list_rows();
        assert_eq!(rows.len(), 1, "{rows:?}");
        let row = &rows[0];
        assert_eq!(
            (
                row.provider.as_str(),
                row.path.as_deref(),
                row.source.as_str(),
                row.field.as_str()
            ),
            ("k:memo", Some("/m"), MEMO_SOURCE, "f")
        );
        assert_eq!(row.value, serde_json::json!(1));
        assert!(row.kind.is_none(), "the server fills it in");
    }
}
