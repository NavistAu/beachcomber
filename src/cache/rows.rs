//! Status-row presentation: the cache as `status` lists it.

use super::*;
use crate::provider::Value;

#[derive(Debug, Clone, serde::Serialize)]
pub struct CacheEntryInfo {
    pub provider: String,
    pub path: Option<String>,
    pub age_ms: u128,
    pub field_count: usize,
}

/// The `status` row and what it carries: the wire's types.
pub use libbeachcomber::protocol::{CacheRow, FailureSnapshot, RowKind};

impl Cache {
    /// List one row per (source, field) across all cache entries, each with
    /// its writer; a row's `source` is that writer presented.
    /// Used by `Request::Status` to return the tabular "what is warm?" payload.
    /// Each row's `age_ms` comes from that field's owning source's last-refreshed timestamp.
    pub fn list_rows(&self) -> Vec<(Origin, CacheRow)> {
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
            let is_cache_facet = matches!(
                libbeachcomber::key::split_facet(&provider),
                Ok((_, Some(Facet::Cache)))
            );
            for (origin, src) in ce.maps() {
                if is_cache_facet && !origin.is_static() {
                    continue;
                }
                let source = origin.presented_label();
                // A memo's value is a row of `<p>:memo`, aged by its own
                // stamp; the keys stored for it in `<p>:cache` are not rows.
                // A memo of absence has no value to show.
                if origin == Origin::Memo {
                    for (address, stamp) in &ce.memos {
                        let Some(v) = crate::provider::lookup_path(&src.fields, address) else {
                            continue;
                        };
                        out.push((
                            origin.clone(),
                            CacheRow {
                                shadowed: false,
                                provider: provider.clone(),
                                path: path.clone(),
                                source: source.clone(),
                                field: address.clone(),
                                value: serde_json::to_value(v).unwrap_or(serde_json::Value::Null),
                                age_ms: Some(stamp.age_ms()),
                                // The server fills in its lifecycle.
                                kind: None,
                                poll_interval_secs: None,
                                poll_count: None,
                                polls_elapsed: None,
                                next_poll_in_secs: None,
                                failure: None,
                            },
                        ));
                    }
                    continue;
                }
                // The `put` map has one row per address, aged by that
                // address's own stamp. The root address lists its top-level
                // keys, one row each, as every namespace's records are.
                if origin == Origin::Put {
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
                            out.push((
                                origin.clone(),
                                CacheRow {
                                    shadowed: false,
                                    provider: provider.clone(),
                                    path: path.clone(),
                                    source: source.clone(),
                                    field,
                                    value: serde_json::to_value(v)
                                        .unwrap_or(serde_json::Value::Null),
                                    age_ms: Some(stamp.age_ms()),
                                    // Only an address written with a ttl is a lifecycle entry; the
                                    // server fills its kind in.
                                    kind: stamp.interval_secs.is_none().then_some(RowKind::Static),
                                    poll_interval_secs: None,
                                    poll_count: None,
                                    polls_elapsed: None,
                                    next_poll_in_secs: None,
                                    failure: None,
                                },
                            ));
                        }
                    }
                    continue;
                }
                let age = src.age_ms();
                // A row of a configuration seed carries no Source instance —
                // it is a static entry.
                let kind = if origin.is_static() {
                    Some(RowKind::Static)
                } else {
                    None
                };
                for (field, v) in &src.fields {
                    // Value implements Serialize with #[serde(untagged)] — to_value is lossless.
                    let value = serde_json::to_value(v).unwrap_or(serde_json::Value::Null);
                    out.push((
                        origin.clone(),
                        CacheRow {
                            shadowed: false,
                            provider: provider.clone(),
                            path: path.clone(),
                            source: source.clone(),
                            // The row's address below the namespace, canonical so it
                            // can be typed back as a key.
                            field: libbeachcomber::key::quote(field).into_owned(),
                            value,
                            age_ms: Some(age),
                            kind: kind.clone(),
                            poll_interval_secs: None,
                            poll_count: None,
                            polls_elapsed: None,
                            next_poll_in_secs: None,
                            failure: None,
                        },
                    ));
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
                let field_count: usize = ce.maps().map(|(_, s)| s.fields.len()).sum();
                CacheEntryInfo {
                    provider,
                    path,
                    age_ms: ce.age_ms(),
                    field_count,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::cache::test_support::*;
    use std::collections::HashMap;

    /// A row seeded from a config file carries no Source instance — `list_rows` must mark it `RowKind::Static`,
    /// same as a `put`-written row.
    #[test]
    fn list_rows_marks_config_seeded_source_static() {
        let cache = Cache::new();
        cache
            .write_static(
                "myapp",
                None,
                &Origin::Config("x.toml".into()),
                "greeting",
                Value::String("hello".into()),
                None,
            )
            .unwrap();

        let rows = cache.list_rows();
        let (_, row) = rows
            .iter()
            .find(|(_, r)| r.provider == "myapp")
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
        cache
            .write_static(
                "myapp",
                None,
                &Origin::Put,
                "greeting",
                Value::String("hello".into()),
                None,
            )
            .unwrap();

        let rows = cache.list_rows();
        assert!(
            !rows.iter().any(|(_, r)| r.kind == Some(RowKind::Rendered)),
            "list_rows must never emit RowKind::Rendered: {rows:?}"
        );
    }

    /// A `put` record and a Source's records share one entry. Evicting the Source's map leaves the
    /// static record — and only the static record — behind.
    #[test]
    fn static_record_survives_its_entry_mate_being_evicted() {
        let cache = Cache::new();

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

    #[test]
    fn status_does_not_list_stored_keys() {
        let cache = Cache::new();
        cache.put_source(
            "p",
            None,
            "s",
            make_fields(&[("a", "1")]),
            &crate::provider::Schedule::poll(5, 1),
            None,
        );
        cache
            .write_static(
                "p",
                None,
                &crate::cache::Origin::Put,
                "x",
                Value::Int(1),
                Some(60),
            )
            .unwrap();
        cache
            .write_static(
                "p:cache",
                None,
                &crate::cache::Origin::Put,
                "_.decay",
                Value::Int(2),
                None,
            )
            .unwrap();

        let mut rows: Vec<(String, String, String)> = cache
            .list_rows()
            .into_iter()
            .map(|(_, r)| (r.provider, r.source, r.field))
            .collect();
        rows.sort();
        assert_eq!(
            rows,
            vec![
                ("p".to_string(), "put".to_string(), "x".to_string()),
                ("p".to_string(), "source(s)".to_string(), "a".to_string()),
                (
                    "p:cache".to_string(),
                    "put".to_string(),
                    "_.decay".to_string()
                ),
            ],
            "the daemon's stored keys describe rows already listed; a prototype is a row of its own"
        );
    }
}
