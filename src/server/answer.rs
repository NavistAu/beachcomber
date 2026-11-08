//! Assembling a read's response: the data half, the template half, the memo half, and the freshness they report.

use super::cache_facet::cache_facet_response;
use super::read::lookup_merged;
use crate::cache::{Cache, MergedRead};
use crate::protocol::Response;
use crate::provider::registry::ProviderRegistry;
use crate::query::{Instance, KeyParse};
use std::collections::HashMap;

/// A data hit, carrying the freshness [`freshness`] found for it.
///
/// Every node of a merged tree has at least one record at or below it, so a hit
/// with no freshness is a bug in the merge rather than a value to report. The
/// fields are omitted rather than filled in with a zero that would read as
/// "just refreshed".
pub(super) fn hit_response(data: serde_json::Value, freshness: Option<u128>) -> Response {
    debug_assert!(
        freshness.is_some(),
        "a data hit with no record behind it: the merge lost provenance"
    );
    match freshness {
        Some(age_ms) => Response::ok(data, age_ms),
        None => Response {
            data: Some(data),
            ..Response::miss()
        },
    }
}

/// The `age_ms` to report for the node at `rest` (`""` for the
/// whole namespace) in a merged read.
///
/// A leaf is answered by its own record's source entry. An object is
/// answered by the records under it together: the largest of their ages.
/// `None` when no record sits at or below the address, which is also when
/// there is no `data` to report freshness for.
pub(super) fn freshness(read: &MergedRead, rest: &str) -> Option<u128> {
    if let Some(record) = read.records.get(rest) {
        return Some(record.age_ms());
    }
    let mut oldest: Option<u128> = None;
    for (tail, record) in &read.records {
        if !libbeachcomber::key::within(tail, rest) {
            continue;
        }
        let age = record.age_ms();
        oldest = Some(oldest.map_or(age, |o| o.max(age)));
    }
    if oldest.is_some() {
        return oldest;
    }
    // Inside a leaf — an array's element: the record is the array's own.
    let mut at = rest.to_string();
    while let Some(up) = libbeachcomber::key::parent(&at) {
        if let Some(record) = read.records.get(&up) {
            return Some(record.age_ms());
        }
        if up.is_empty() {
            break;
        }
        at = up;
    }
    None
}

/// What the template half of a `get` found, and the memo half beside it.
/// `Default` is "nothing attached", which is every early return in
/// [`template_half`].
#[derive(Default)]
pub(super) struct TemplateHalf {
    /// The node at the requested address in the `template` facet's merged tree.
    pub(super) template: Option<serde_json::Value>,
    /// The part of the address the walk could not consume.
    pub(super) rest: Option<String>,
    /// The address `template` stands at: the requested one, or that of the
    /// string the walk stopped on.
    at: String,
    /// The memos of the template's leaves ([`memo_half`]).
    memo: Option<serde_json::Value>,
}

/// What a `get` answers once the provider's `return` has read the data: the
/// data it hands out, and the template half beside it.
pub(super) struct Answered {
    pub(super) merged: Option<MergedRead>,
    pub(super) half: TemplateHalf,
}

/// The provider's `return` over `result`, the data its records gave (spec
/// §3.4): the data it answers, and the template half, the node at `rest` in
/// the tree the line attaches as the `template` facet.
///
/// The line is read once per answer, over the data read (`result`) and the
/// merged tree of `provider:template` at the same address (`template`), which
/// is a static record: it resolves up the chain from the one path the caller
/// named, and no instance is involved. A return that never calls
/// `facet('template', ...)` attaches nothing, and so does one that fails to
/// render (logged once); one that fails leaves the data as it was.
///
/// A group (object) or a value expression (string) at the address is the
/// template. A string reached with segments still to go is the template too,
/// and the remainder comes back as `rest` for the client to apply to whatever
/// the expression renders. Anything else — a miss, or a non-string scalar in
/// the way — attaches nothing.
pub(super) fn answer(
    cache: &Cache,
    registry: &ProviderRegistry,
    provider: &str,
    rest: &str,
    path: Option<&str>,
    result: Option<MergedRead>,
) -> Answered {
    let returned = crate::shape::provider::returned_over(cache, registry, provider, path, result);
    let mut half = template_half(returned.template, rest);
    if let Some(template) = &half.template
        && cache.has_memos(provider)
    {
        half.memo = memo_half(cache, provider, path, template, &half.at);
    }
    Answered {
        half,
        merged: returned.read,
    }
}

/// The memo half for `template`, which stands at `at`: for a string, the
/// memo of that leaf a read from `path` finds ([`Cache::memo_record`]); for a
/// group, the tree of its leaves' memos, a leaf with none left out. `None`
/// when no leaf has one.
pub(super) fn memo_half(
    cache: &Cache,
    provider: &str,
    path: Option<&str>,
    template: &serde_json::Value,
    at: &str,
) -> Option<serde_json::Value> {
    match template {
        serde_json::Value::String(_) => cache
            .memo_record(provider, path, at)
            .map(|memo| memo.half()),
        serde_json::Value::Object(group) => {
            let tree: serde_json::Map<String, serde_json::Value> = group
                .iter()
                .filter_map(|(k, t)| {
                    let below = libbeachcomber::key::push(at, k);
                    Some((k.clone(), memo_half(cache, provider, path, t, &below)?))
                })
                .collect();
            (!tree.is_empty()).then(|| tree.into())
        }
        _ => None,
    }
}

/// The node at `rest` in the `template` facet a `return` attached.
pub(super) fn template_half(
    attached: Option<HashMap<String, crate::provider::Value>>,
    rest: &str,
) -> TemplateHalf {
    use crate::provider::Value;
    let Some(fields) = attached else {
        return TemplateHalf::default();
    };
    let read = MergedRead {
        fields,
        records: HashMap::new(),
    };

    if rest.is_empty() {
        return TemplateHalf {
            template: serde_json::to_value(&read.fields).ok(),
            ..TemplateHalf::default()
        };
    }

    let Some((head, mut left)) = libbeachcomber::key::split_first(rest) else {
        return TemplateHalf::default();
    };
    let Some(mut current) = read.fields.get(&head) else {
        return TemplateHalf::default();
    };
    let mut at = libbeachcomber::key::push("", &head);
    while let Some(remainder) = left {
        let Some((segment, below)) = libbeachcomber::key::split_first(&remainder) else {
            return TemplateHalf::default();
        };
        match current {
            Value::Object(map) => match map.get(&segment) {
                Some(v) => current = v,
                None => return TemplateHalf::default(),
            },
            // A value expression with the address unconsumed: the client
            // renders it and walks the remainder into the result.
            Value::String(_) => {
                return TemplateHalf {
                    template: serde_json::to_value(current).ok(),
                    rest: Some(remainder),
                    at,
                    memo: None,
                };
            }
            _ => return TemplateHalf::default(),
        }
        at = libbeachcomber::key::push(&at, &segment);
        left = below;
    }
    match current {
        Value::Object(_) | Value::String(_) => TemplateHalf {
            template: serde_json::to_value(current).ok(),
            rest: None,
            at,
            memo: None,
        },
        _ => TemplateHalf::default(),
    }
}

/// Assemble the response for a resolved read: the data half out of `merged`
/// with its own freshness, and the template and memo halves attached to it.
///
/// `get` and `watch` share this, so a streamed value and a fetched one cannot
/// answer the same key differently. The only thing they do apart is warming:
/// `get` runs sources on a cold miss, `watch` waits for the scheduler.
///
/// An address nothing answers is a miss, at any depth: `p.project.nonesuch`
/// where `project` exists but has no `nonesuch`, or `p.lit.x` where `lit` is a
/// scalar, miss exactly as `p.nonesuch` does. That is the same rule the client
/// applies when it walks an address past a template (a scalar reached with
/// segments left is a miss), so the two halves of the tree agree.
pub(super) fn resolve_halves(
    merged: &Option<MergedRead>,
    target: &KeyParse,
    half: TemplateHalf,
) -> Response {
    let rest = target.rest();
    let response = match target {
        KeyParse::Field(..) => match (lookup_merged(merged, rest), merged) {
            (Some(data), Some(m)) => hit_response(data, freshness(m, rest)),
            _ => Response::miss(),
        },
        KeyParse::Provider(_) => match merged {
            Some(m) => serde_json::to_value(&m.fields)
                .ok()
                .map_or_else(Response::miss, |data| hit_response(data, freshness(m, ""))),
            None => Response::miss(),
        },
    };
    response.with_halves(half.template, half.rest, half.memo)
}

// Watch reads only from cache — it starts no run on a cold miss (unlike Get).
// A cache miss in a streaming context means the scheduler has not populated the
// entry yet; the caller receives an update when it does.
pub(super) fn read_watch_value(
    cache: &Cache,
    registry: &ProviderRegistry,
    target: &KeyParse,
    requested: Option<&str>,
    reached: &[Instance],
    layers: crate::cache::Layers,
) -> Response {
    if layers == crate::cache::Layers::Cache {
        return cache_facet_response(cache, registry, target, requested, reached);
    }
    let (KeyParse::Field(provider, _) | KeyParse::Provider(provider)) = target;
    // A watch frame carries the same two halves a `get` does.
    let merged = cache.read_merged(provider, requested, reached, registry, layers);
    let Answered { merged, half } = answer_read(cache, registry, target, requested, layers, merged);
    resolve_halves(&merged, target, half)
}

/// What a read of `target` from `requested` answers over `merged`, the data
/// its instances gave: the one step after warming that `get` and `watch`
/// share. A layer facet, or a key already inside a facet, is a plain read and
/// gets no template half; any other is answered through the provider's
/// `return` ([`answer`]), whose template decides whether an unresolvable
/// sub-path is a hit or a miss. Templates are static records, so the template
/// half resolves up the chain from the directory the caller named, not from an
/// instance's root.
pub(super) fn answer_read(
    cache: &Cache,
    registry: &ProviderRegistry,
    target: &KeyParse,
    requested: Option<&str>,
    layers: crate::cache::Layers,
    merged: Option<MergedRead>,
) -> Answered {
    let (KeyParse::Field(provider, _) | KeyParse::Provider(provider)) = target;
    if layers != crate::cache::Layers::All
        || crate::query::parse_facet(provider).ok().flatten().is_some()
    {
        return Answered {
            merged,
            half: TemplateHalf::default(),
        };
    }
    answer(cache, registry, provider, target.rest(), requested, merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_watch_value_field_target_finds_field() {
        use crate::cache::Cache;
        use crate::provider::Value;
        use crate::query::KeyParse;
        use std::collections::HashMap;

        let cache = Cache::new();
        let mut refs_fields = HashMap::new();
        refs_fields.insert("branch".to_string(), Value::String("main".to_string()));
        cache.put_source(
            "git",
            Some("/repo"),
            "refs",
            refs_fields,
            crate::cache::Schedule::default(),
        );

        let registry = ProviderRegistry::shipped(&std::env::vars().collect());
        let target = KeyParse::Field("git".to_string(), "branch".to_string());
        let resp = read_watch_value(
            &cache,
            &registry,
            &target,
            Some("/repo"),
            &[Instance::new("refs", Some("/repo"))],
            crate::cache::Layers::All,
        );
        assert!(resp.ok);
        assert_eq!(
            resp.data.unwrap(),
            serde_json::Value::String("main".to_string())
        );
    }

    /// Freshness is per record, not per entry: a leaf reports its own record's
    /// source entry, and an object reports the largest age among the records
    /// under it and is stale if any one of them is.
    ///
    /// Two static maps in one entry, written five seconds apart with TTLs that
    /// make the first stale and the second fresh. The clock is paused so the
    /// gap is exactly five seconds every run.
    #[tokio::test(start_paused = true)]
    async fn freshness_is_the_record_s_own_then_the_oldest_of_an_object_s() {
        use crate::cache::Cache;
        use crate::provider::Value;

        let cache = Cache::new();
        let registry = ProviderRegistry::new();
        // `put` carrying `--ttl 1s`, and a config seed with a long one.
        cache
            .write_static(
                "p",
                None,
                &crate::cache::Origin::Put,
                "x",
                Value::Int(1),
                Some(1),
            )
            .expect("put accepted");
        tokio::time::advance(std::time::Duration::from_secs(5)).await;
        cache
            .write_static(
                "p",
                None,
                &crate::cache::Origin::Config("t.toml".into()),
                "",
                Value::Object([("y".to_string(), Value::Int(2))].into_iter().collect()),
                Some(100),
            )
            .expect("put accepted");

        let read = cache
            .read_merged("p", None, &[], &registry, crate::cache::Layers::All)
            .expect("both maps");

        assert_eq!(
            freshness(&read, "x"),
            Some(5000),
            "the older record's own age"
        );
        assert_eq!(freshness(&read, "y"), Some(0), "the newer record's own age");
        assert_eq!(
            freshness(&read, ""),
            Some(5000),
            "the assembled object takes the largest age"
        );
        assert_eq!(freshness(&read, "nope"), None, "no record, no freshness");
    }

    /// An empty object is a record at its own tail, so a farther layer's `{}`
    /// used to leave provenance standing at a node a nearer layer had filled —
    /// and the node then reported the empty record's age instead of the age of
    /// what actually answers there.
    #[tokio::test(start_paused = true)]
    async fn a_shadowed_empty_object_leaves_no_phantom_provenance() {
        use crate::cache::Cache;
        use crate::provider::Value;
        use std::collections::HashMap;

        let cache = Cache::new();
        let registry = ProviderRegistry::new();
        // The far layer: the global entry, holding `tree` as an empty node.
        cache
            .write_static(
                "p",
                None,
                &crate::cache::Origin::Put,
                "tree",
                Value::Object(HashMap::new()),
                None,
            )
            .expect("put accepted");
        tokio::time::advance(std::time::Duration::from_secs(9)).await;
        // The near layer: a directory below it, filling that node in.
        cache
            .write_static(
                "p",
                Some("/x/y"),
                &crate::cache::Origin::Put,
                "tree.left",
                Value::Int(1),
                None,
            )
            .expect("put accepted");

        let read = cache
            .read_merged("p", Some("/x/y"), &[], &registry, crate::cache::Layers::All)
            .expect("both layers");

        assert_eq!(
            freshness(&read, "tree"),
            Some(0),
            "the node reports the record that answers it, not the empty one it replaced"
        );
        assert_eq!(
            freshness(&read, ""),
            Some(0),
            "and so does the root the node sits in"
        );
    }
}
