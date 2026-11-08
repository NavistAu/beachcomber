//! The schedule a Source's records carry into the `<p>:cache` facet.

use super::*;
use crate::provider::Value;
use std::collections::HashMap;
use tokio::time::Instant;

/// What a Source's records carry into the `<p>:cache` facet beside them: the
/// schedule that keeps them fresh, and the validity of the run that made
/// them. Built from the Source's metadata ([`Schedule::of`]) and, by the run,
/// [`Schedule::with_validity`], so a record and what is known of it are
/// written together and removed together.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Schedule {
    /// `(interval_secs, count)` for a Source that polls.
    pub poll: Option<(u64, u32)>,
    /// The watch patterns, then the absolute paths, for a Source that watches.
    pub watch: Vec<String>,
    /// The Source's own decay depth (`ttl(...)` sets 1), stored as `decay`.
    pub decay: Option<u8>,
    /// The record's stat inputs as its run found them, stored as `hash`;
    /// `None` for a Source with none (one that only polls).
    pub validity: Option<Validity>,
}

impl Schedule {
    pub fn poll(interval_secs: u64, count: u32) -> Self {
        Self {
            poll: Some((interval_secs, count)),
            watch: Vec::new(),
            decay: None,
            validity: None,
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

    /// The schedule, with the validity of the run whose records it carries.
    pub fn with_validity(self, validity: Option<Validity>) -> Self {
        Self { validity, ..self }
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
                validity: None,
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
                validity: None,
            },
        }
    }

    /// The stored `:cache` keys of one record: `poll`, `watch`, `decay` and
    /// `hash` (16 hex digits).
    pub(super) fn stored_keys(&self) -> HashMap<String, Value> {
        let mut keys = HashMap::new();
        if let Some(validity) = &self.validity {
            keys.insert(
                "hash".to_string(),
                Value::String(libbeachcomber::validity::hex(validity.hash)),
            );
        }
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

pub(super) fn poll_key(interval_secs: u64, count: u32) -> Value {
    Value::Object(HashMap::from([
        (
            "interval".to_string(),
            Value::String(format!("{interval_secs}s")),
        ),
        ("count".to_string(), Value::Int(i64::from(count))),
    ]))
}

/// Where a put at `node` keeps its stored keys in `<namespace>:cache`: the
/// node's own address, or for a root put (which has none) each top-level key;
/// never a `_` prototype.
pub(super) fn schedule_addresses(node: &str, value: &Value) -> Vec<String> {
    let addresses: Vec<String> = match (node, value) {
        ("", Value::Object(top)) => top.keys().cloned().collect(),
        _ => vec![node.to_string()],
    };
    addresses.into_iter().filter(|a| !is_prototype(a)).collect()
}

impl Cache {
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
        let validity;
        let fields: Vec<String> = {
            let Some(mut entry) = self.entries.get_mut(&make_cache_key(provider, path)) else {
                return;
            };
            let Some(src) = entry.sources.get_mut(source_name) else {
                return;
            };
            src.expected_interval_secs = schedule.poll.map(|(secs, _)| secs);
            // The run's validity is the record's, not the schedule's: it
            // stays, and with it the stored `hash`.
            validity = src.validity.clone();
            src.fields.keys().cloned().collect()
        };
        let facet_key = make_cache_key(&facet_of(provider, Facet::Cache), path);
        let stored = schedule.with_validity(validity).stored_keys();
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
                    validity: None,
                },
            );
        }
        if let Some(ref watchers) = self.watchers {
            watchers.notify(provider, path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::cache::test_support::*;

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
}
