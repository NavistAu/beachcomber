//! The `p:cache` facet of `get` and the `layers` op: what is stored, and where, without running anything.

use super::Ctx;
use super::answer::{Answered, answer};
use super::read::all_instances;
use crate::cache::{Cache, Layers};
use crate::protocol::Response;
use crate::provider::registry::ProviderRegistry;
use crate::provider::source_call::instance_key;
use crate::query::{Instance, Key, QueryPlan};
use std::sync::Arc;
use tracing::warn;

/// The answer of `get p:cache.<path>`: the tree of what is known about the
/// records a read of `p.<path>` resolves to, shaped like that read's value, with
/// a leaf answer at each leaf. The address resolves exactly as the read does
/// (same chain, nearest record), and nothing is warmed or run.
///
/// A leaf answers `age_ms` and `source` (derived from the record that
/// answered) and, for a Source's record, `executing` (a run of its instance is
/// in flight), plus the keys stored beside it (`poll`, `watch`, `decay`). A leaf a
/// template answers is `{"source": "template"}`, with no
/// `age_ms`: the client renders it, so it is as old as the inputs the render
/// reads, which the daemon never sees. A template leaf with a memo answers for
/// the memo instead ([`crate::cache::MemoRecord::cache_answer`]): `source`
/// `memo`, its age, and its stored keys. A Source's record or a memo with no
/// `decay` stored answers the depth its lifecycle runs at
/// ([`Cache::resolve_decay`]). `p:cache._` is the prototype node,
/// which is not part of the value: it reads back as written.
pub(super) fn cache_facet_response(
    cache: &Cache,
    registry: &ProviderRegistry,
    target: &Key,
    requested: Option<&str>,
    reached: &[Instance],
) -> Response {
    let provider = &target.provider;
    let rest = target.rest();

    if rest == "_" || rest.starts_with("_.") {
        let prototypes = cache.read_merged(
            &crate::cache::facet_of(provider, libbeachcomber::key::Facet::Cache),
            requested,
            &[],
            registry,
            Layers::All,
        );
        return match prototypes.and_then(|m| {
            crate::provider::lookup_path(&m.fields, rest).and_then(|v| serde_json::to_value(v).ok())
        }) {
            Some(value) => Response::ok(value, 0),
            None => Response::miss(),
        };
    }

    // A key below a leaf (`p:cache.f.age_ms`) reads into that leaf's answer:
    // the deepest node of the key the read reaches answers, and the rest of
    // the key walks into that answer.
    let segments = &target.path;
    let answered = (segments.len().min(1)..=segments.len())
        .rev()
        .find_map(|n| {
            let tree = cache_facet_tree(
                cache,
                registry,
                provider,
                &segments[..n],
                requested,
                reached,
            )?;
            Some((tree, libbeachcomber::key::format_address(&segments[n..])))
        });
    match answered {
        Some((tree, left)) if left.is_empty() => Response::ok(tree, 0),
        Some((tree, left)) => libbeachcomber::resolve::walk(tree, &left)
            .map_or_else(Response::miss, |v| Response::ok(v, 0)),
        None => Response::miss(),
    }
}

/// The `:cache` tree at `path`, an address's segments, or `None` when the read does not reach a node
/// there ([`cache_facet_response`]).
pub(super) fn cache_facet_tree(
    cache: &Cache,
    registry: &ProviderRegistry,
    provider: &str,
    path: &[String],
    requested: Option<&str>,
    reached: &[Instance],
) -> Option<serde_json::Value> {
    let rest = &libbeachcomber::key::format_address(path);
    let merged = cache.read_merged(provider, requested, reached, registry, Layers::All);
    let Answered { merged, half } = answer(cache, registry, provider, path, requested, merged);
    let value = merged.as_ref().and_then(|m| {
        if path.is_empty() {
            Some(crate::provider::Value::Object(m.fields.clone()))
        } else {
            crate::provider::lookup_segments(&m.fields, path).cloned()
        }
    });
    // A template string reached with key left over answers at its own, shallower
    // node, as a read renders it there.
    if half.rest.is_some() || (value.is_none() && half.template.is_none()) {
        return None;
    }
    // A lifecycle entry without a decay of its own decays at the prototype's
    // depth, as `source_lifecycle_config` and `static_lifecycle_config` resolve it.
    let inherit_decay = |answer: &mut serde_json::Map<String, serde_json::Value>,
                         entry: Option<&str>| {
        answer
            .entry("decay")
            .or_insert_with(|| cache.resolve_decay(provider, entry).into());
    };
    let leaf = |tail: &str| -> Option<serde_json::Value> {
        let record = merged.as_ref()?.records.get(tail)?;
        let mut answer = serde_json::Map::new();
        answer.insert("age_ms".into(), (record.age_ms() as u64).into());
        let origin = &record.source;
        answer.insert("source".into(), origin.presented_label().into());
        // A Source's record is being brought up to date while a run of its
        // instance is in flight, whoever started it.
        if let Some(name) = origin.source_name() {
            let instance = instance_key(provider, name, record.entry.as_deref());
            answer.insert(
                "executing".into(),
                registry.runs().is_running(&instance).into(),
            );
        }
        let stored = cache.stored_keys(provider, record.entry.as_deref(), origin, tail);
        for (key, v) in stored.iter().flatten() {
            if let Ok(v) = serde_json::to_value(v) {
                answer.insert(key.clone(), v);
            }
        }
        if !origin.is_static() {
            inherit_decay(&mut answer, record.entry.as_deref());
        }
        Some(answer.into())
    };

    let template_leaf = |tail: &str| -> serde_json::Value {
        match cache.memo_record(provider, requested, tail) {
            // Every memo is a lifecycle entry (`apply_memo_written`).
            Some(memo) => {
                let mut answer = memo.cache_answer();
                inherit_decay(&mut answer, memo.entry.as_deref());
                answer.into()
            }
            None => serde_json::json!({"source": "template"}),
        }
    };

    cache_facet_node(
        rest,
        value.as_ref(),
        half.template.as_ref(),
        &leaf,
        &template_leaf,
    )
}

/// The tree of leaf answers for the node at `tail`: the one place the answer
/// for an object node lives. `value` is the data at the node, `template` the
/// template tree at it; a template wins over data at the same leaf, as it does
/// in a read, and `template_leaf` answers for it. An object with no answer
/// anywhere below it is no answer.
pub(super) fn cache_facet_node(
    tail: &str,
    value: Option<&crate::provider::Value>,
    template: Option<&serde_json::Value>,
    leaf: &dyn Fn(&str) -> Option<serde_json::Value>,
    template_leaf: &dyn Fn(&str) -> serde_json::Value,
) -> Option<serde_json::Value> {
    use crate::provider::Value;
    let child = |key: &str| libbeachcomber::key::push(tail, key);
    match (value, template) {
        (_, Some(serde_json::Value::Object(group))) => {
            let data = match value {
                Some(Value::Object(map)) => Some(map),
                _ => None,
            };
            let mut keys: Vec<&String> = group
                .keys()
                .chain(data.into_iter().flat_map(|m| m.keys()))
                .collect();
            keys.sort();
            keys.dedup();
            let tree: serde_json::Map<String, serde_json::Value> = keys
                .into_iter()
                .filter_map(|k| {
                    let answer = cache_facet_node(
                        &child(k),
                        data.and_then(|m| m.get(k)),
                        group.get(k),
                        leaf,
                        template_leaf,
                    )?;
                    Some((k.clone(), answer))
                })
                .collect();
            (!tree.is_empty()).then(|| tree.into())
        }
        (_, Some(_)) => Some(template_leaf(tail)),
        (Some(Value::Object(map)), None) if !map.is_empty() => {
            let tree: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .filter_map(|(k, v)| {
                    Some((
                        k.clone(),
                        cache_facet_node(&child(k), Some(v), None, leaf, template_leaf)?,
                    ))
                })
                .collect();
            (!tree.is_empty()).then(|| tree.into())
        }
        (Some(_), None) => leaf(tail),
        (None, None) => None,
    }
}

/// The `layers` answer for `plan`: per leaf of the node the key names, every
/// layer a read from the requested directory consults, in order — memo
/// records, template records, then a leaf a provider line computed, then each
/// link's `put`, `config(…)` seeds and Source records ([`Cache::layer_stacks`])
/// — with the one the read takes marked `answered`: at a template leaf the
/// nearest memo while it is live, else the template. A leaf's list also holds a record
/// standing as a scalar on an ancestor of it, which hides the leaf in that
/// layer. A tree shaped like the value, an array at each leaf; a leaf key is
/// its array alone.
pub(super) fn layers_response(
    cache: &Cache,
    registry: &ProviderRegistry,
    plan: &QueryPlan,
    instances: &[Instance],
) -> Response {
    use crate::cache::LayerRecord;
    let provider = plan.provider.as_str();
    let requested = plan.requested_path.as_deref();
    let merged = cache.read_merged(provider, requested, instances, registry, Layers::All);
    let Answered { merged, half } = answer(cache, registry, provider, &[], requested, merged);

    // The address of the template string that answers `tail`: `tail`
    // itself, or the string above it the walk stops on.
    let template_string = |tail: &str| -> Option<String> {
        let mut node = half.template.as_ref();
        let mut at = String::new();
        for segment in libbeachcomber::key::segments(tail) {
            match node {
                Some(serde_json::Value::String(_)) => return Some(at),
                Some(serde_json::Value::Object(map)) => node = map.get(segment.as_ref()),
                _ => return None,
            }
            at = libbeachcomber::key::push(&at, &segment);
        }
        matches!(node, Some(serde_json::Value::String(_))).then_some(at)
    };

    let stacked = cache.layer_stacks(provider, requested, instances);
    // A memo is read only through its template: one at an address no template
    // string stands at is no layer of anything.
    let (mut ordered, data): (Vec<_>, Vec<_>) = stacked
        .into_iter()
        .filter(|(tail, r)| {
            r.facet.as_deref() != Some("memo") || template_string(tail).as_deref() == Some(tail)
        })
        .partition(|(_, r)| r.facet.is_some());
    // A leaf a provider line computed belongs to no record: it sits under
    // the templates and over the data it was computed from.
    if let Some(m) = &merged {
        let mut computed: Vec<(&String, &crate::cache::RecordFreshness)> = m
            .records
            .iter()
            .filter(|(_, r)| matches!(r.source, crate::cache::Origin::Computed(_)))
            .collect();
        computed.sort_by(|a, b| a.0.cmp(b.0));
        for (tail, record) in computed {
            let Some(value) = crate::provider::lookup_path(&m.fields, tail) else {
                continue;
            };
            ordered.push((
                tail.clone(),
                LayerRecord::new(
                    &record.source,
                    None,
                    record.entry.as_deref(),
                    value,
                    record.age_ms(),
                ),
            ));
        }
    }
    ordered.extend(data);

    // Tails are canonical addresses: `a['b.c']` lies under `a`.
    let under = |tail: &str, at: &str| tail != at && libbeachcomber::key::within(tail, at);
    let mut tails: Vec<&str> = ordered.iter().map(|(t, _)| t.as_str()).collect();
    tails.sort();
    tails.dedup();
    let leaves: Vec<&str> = tails
        .iter()
        .copied()
        .filter(|t| !tails.iter().any(|other| under(other, t)))
        .collect();

    let answering = |tail: &str| -> Option<(String, Option<String>)> {
        let records = &merged.as_ref()?.records;
        let mut at = tail.to_string();
        loop {
            if let Some(r) = records.get(&at) {
                return Some((
                    r.source.presented_label(),
                    r.entry.as_deref().map(str::to_string),
                ));
            }
            at = libbeachcomber::key::parent(&at).filter(|p| !p.is_empty())?;
        }
    };

    let rest = plan.target.rest();
    let mut tree = serde_json::Map::new();
    let mut alone: Option<serde_json::Value> = None;
    for leaf in leaves {
        if !rest.is_empty() && leaf != rest && !under(leaf, rest) {
            continue;
        }
        let mut list: Vec<LayerRecord> = ordered
            .iter()
            .filter(|(t, _)| t == leaf || under(leaf, t))
            .map(|(_, r)| r.clone())
            .collect();
        if let Some(string) = template_string(leaf) {
            let live = cache
                .memo_record(provider, requested, &string)
                .filter(crate::cache::MemoRecord::is_live);
            let memo = live.and_then(|memo| {
                list.iter()
                    .position(|r| r.facet.as_deref() == Some("memo") && r.path == memo.entry)
            });
            let template = list
                .iter()
                .position(|r| r.facet.as_deref() == Some("template"));
            if let Some(i) = memo.or(template) {
                list[i].answered = true;
            }
        } else if let Some((label, path)) = answering(leaf)
            && let Some(hit) = list
                .iter_mut()
                .find(|r| r.facet.is_none() && r.label == label && r.path == path)
        {
            hit.answered = true;
        }
        let list = serde_json::to_value(list).unwrap_or_default();
        if leaf == rest {
            alone = Some(list);
            continue;
        }
        let below = libbeachcomber::key::segments(rest).len();
        let relative: Vec<_> = libbeachcomber::key::segments(leaf)
            .into_iter()
            .skip(below)
            .collect();
        insert_at(&mut tree, &relative, list);
    }
    match alone {
        Some(list) => Response::ok(list, 0),
        None if tree.is_empty() => Response::miss(),
        None => Response::ok(serde_json::Value::Object(tree), 0),
    }
}

/// Set `value` at the address `segments` of `tree`, making the objects on
/// the way. Each segment is a key as stored, never split again.
pub(super) fn insert_at<S: AsRef<str>>(
    tree: &mut serde_json::Map<String, serde_json::Value>,
    segments: &[S],
    value: serde_json::Value,
) {
    match segments {
        [] => {}
        [last] => {
            tree.insert(last.as_ref().to_string(), value);
        }
        [head, below @ ..] => {
            let child = tree
                .entry(head.as_ref().to_string())
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
            if let serde_json::Value::Object(map) = child {
                insert_at(map, below, value);
            }
        }
    }
}

/// `layers`: the records behind a key, without warming anything.
pub(super) async fn handle_layers(
    ctx: &Ctx,
    registry: &Arc<ProviderRegistry>,
    key: &str,
    path: Option<&str>,
) -> Response {
    let cache = &ctx.cache;
    let requested = path;
    let plan = match QueryPlan::build(key, requested, registry) {
        Ok(plan) => plan,
        Err(msg) => return Response::error(msg),
    };
    match crate::query::parse_facet(&plan.provider) {
        Ok(None) if plan.layers == crate::cache::Layers::All => {}
        Ok(_) => {
            return Response::error(format!(
                "layers takes a provider or field key, not a facet: {key}"
            ));
        }
        Err(msg) => return Response::error(msg),
    }
    // Like `p:cache`: the instances a read would reach, found
    // without warming any of them or signalling demand.
    let instances = match all_instances(registry, &plan).await {
        Ok(instances) => instances,
        Err(msg) => {
            warn!("layers {key}: {msg}");
            return Response::error(msg);
        }
    };
    layers_response(cache, registry, &plan, &instances)
}
