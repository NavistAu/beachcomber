//! `status`: every cached row with its Source's lifecycle and failure state.

use super::Ctx;
use crate::protocol::Response;
use crate::provider::registry::ProviderRegistry;
use std::sync::Arc;

/// `status`: every cached row with its Source's lifecycle and failure state.
pub(super) async fn handle_status(ctx: &Ctx, registry: &Arc<ProviderRegistry>) -> Response {
    let (cache, scheduler) = (&ctx.cache, &ctx.scheduler);
    let mut rows = cache.list_rows();

    let lifecycle = scheduler.get_lifecycle_snapshots().await;
    let failures = scheduler.get_failure_states().await;

    // A Source that has failed every run has stored nothing, so it
    // has no row: give it an absent one per field it declares, which
    // the loop below fills with its lifecycle and its failure. A
    // whole-node Source's fields are unknown until it runs; its one
    // row names its sentinel (`<node>`), as `introspect providers`
    // lists it.
    for key in failures.keys() {
        let has_row = rows
            .iter()
            .any(|(o, r)| r.provider == key.provider && r.path == key.path && *o == key.source);
        if !key.node.is_empty() || has_row {
            continue;
        }
        let fields = registry
            .source_metadata(&key.provider, key.source.name())
            .map(|sm| sm.fields.iter().map(|f| f.name.clone()).collect())
            .unwrap_or_else(|| vec![String::new()]);
        for field in fields {
            rows.push((
                key.source.clone(),
                crate::cache::CacheRow {
                    shadowed: false,
                    provider: key.provider.clone(),
                    path: key.path.clone(),
                    source: key.source.presented_label(),
                    field,
                    value: serde_json::Value::Null,
                    age_ms: None,
                    kind: None,
                    poll_interval_secs: None,
                    poll_count: None,
                    polls_elapsed: None,
                    next_poll_in_secs: None,
                    failure: None,
                },
            ));
        }
    }

    use crate::cache::RowKind;
    for (origin, row) in rows.iter_mut() {
        // `Cache::list_rows` already marked static rows (a put's or a
        // configuration seed's) with RowKind::Static; only rows
        // backed by a real Source need a lifecycle/transient lookup.
        // A put row is looked up only when its address carries a ttl
        // (`Cache::list_rows` leaves its kind unset); without one it
        // stays static, whatever put sits above it.
        let is_put = *origin == crate::cache::Origin::Put;
        // A memo row is a `<p>:memo` row; its lifecycle is `<p>`'s,
        // at its own address.
        let is_memo = *origin == crate::cache::Origin::Memo;

        if row.kind != Some(RowKind::Static) {
            // Look up the lifecycle snapshot for THIS row's owning source
            // so glyphs and TTL columns reflect the source's strategy
            // (refs vs diff vs status all differ within a single provider).
            // A put row's key is its own address: the row's field, or the
            // root address when the field is a root put's top-level key.
            let key = crate::scheduler::lifecycle::Key {
                provider: if is_memo {
                    libbeachcomber::key::split_facet(&row.provider)
                        .map_or(row.provider.as_str(), |(base, _)| base)
                        .to_string()
                } else {
                    row.provider.clone()
                },
                path: row.path.clone(),
                source: origin.clone(),
                node: if is_put || is_memo {
                    row.field.clone()
                } else {
                    String::new()
                },
            };
            let matching_snap = lifecycle.get(&key).or_else(|| {
                is_put
                    .then(|| crate::scheduler::lifecycle::Key {
                        node: String::new(),
                        ..key.clone()
                    })
                    .and_then(|root| lifecycle.get(&root))
            });

            if let Some(snap) = matching_snap {
                row.kind = Some(RowKind::Lifecycle {
                    decay: snap.decay,
                    depth: snap.depth,
                    watches_files: snap.watches_files,
                });
                // Only expose poll-related metadata when this source has
                // a poll path. Pure Watch sources leave these None so the
                // renderer doesn't fabricate `0s×00`.
                if snap.poll_interval_secs > 0 {
                    row.poll_interval_secs = Some(snap.poll_interval_secs);
                    row.poll_count = snap.poll_count;
                    row.polls_elapsed = Some(snap.polls_elapsed);
                    row.next_poll_in_secs = snap.next_poll_in_secs;
                }
            } else if is_put || is_memo {
                row.kind = Some(RowKind::Static);
            } else {
                row.kind = Some(RowKind::Transient);
            }

            // Failure backoff: per-source.
            if let Some(snap) = failures.get(&key) {
                row.failure = Some(snap.clone());
            }
        }
    }

    // Each row's origin keyed the lookups above; a reader gets the
    // presented form `list_rows` wrote, as a `:cache` answer does.
    let mut rows: Vec<crate::cache::CacheRow> = rows.into_iter().map(|(_, row)| row).collect();
    // One order on the wire, not the cache's hash order: a client's stable
    // sort then breaks its ties the same way every time.
    rows.sort_by(|a, b| {
        (&a.provider, &a.path, &a.source, &a.field).cmp(&(
            &b.provider,
            &b.path,
            &b.source,
            &b.field,
        ))
    });
    match serde_json::to_value(&rows) {
        Ok(v) => Response::ok(v, 0),
        Err(e) => Response::error(format!("serialization failed: {e}")),
    }
}
