//! `put` and `memo`: writing static records and memoised template values.

use super::Ctx;
use crate::cache::Cache;
use crate::protocol::Response;
use crate::provider::registry::ProviderRegistry;
use crate::scheduler::SchedulerHandle;
use std::sync::Arc;

/// A request's `ttl`: whole seconds and at least one, or none. One reading
/// for a `put` and a `memo`.
pub(super) fn parse_ttl(ttl: Option<&str>) -> Result<Option<u64>, String> {
    let Some(t) = ttl else {
        return Ok(None);
    };
    let d = libbeachcomber::duration::parse(t)
        .ok_or_else(|| format!("ttl: {}", libbeachcomber::duration::refuse_parse(t)))?;
    libbeachcomber::duration::whole_seconds(d)
        .map(Some)
        .map_err(|e| format!("ttl: {e}"))
}

/// What a `memo` request writes, checked: the provider, the directory, the
/// leaf's canonical address and the memo; or why it is refused. The key is
/// a provider's field, never a facet or a bare provider; the hash sixteen
/// lowercase hex digits; each watch path absolute; the ttl as a `put`'s.
#[allow(clippy::too_many_arguments)]
pub(super) fn memo_write(
    key: &str,
    path: Option<&str>,
    hash: &str,
    data: Option<&serde_json::Value>,
    watch: &[String],
    ttl: Option<&str>,
    log: &[libbeachcomber::protocol::LogLine],
    home: &crate::query::Home,
) -> Result<(String, Option<String>, String, crate::cache::MemoWrite), String> {
    let (provider, field) = libbeachcomber::key::split_key(key)?;
    libbeachcomber::key::check_head(provider)?;
    if libbeachcomber::key::split_facet(provider)?.1.is_some() {
        return Err(format!(
            "memo takes a provider.field key, not a facet: {key}"
        ));
    }
    if field.is_empty() {
        return Err(format!(
            "memo takes a provider.field key: {key} names no field"
        ));
    }
    if libbeachcomber::key::segments(&field).len() > crate::provider::MAX_JSON_DEPTH {
        return Err(format!(
            "memo key is nested too deeply (limit {} segments)",
            crate::provider::MAX_JSON_DEPTH
        ));
    }
    let hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
    if hash.len() != 16 || !hash.bytes().all(hex) {
        return Err(format!(
            "invalid memo hash: {hash} (sixteen lowercase hex digits)"
        ));
    }
    if let Some(relative) = watch
        .iter()
        .find(|p| !std::path::Path::new(p).is_absolute())
    {
        return Err(format!("a memo's watch paths are absolute: {relative}"));
    }
    let ttl_secs = parse_ttl(ttl)?;
    let path = crate::query::Dir::requested(path, home)?.into_address();
    let write = crate::cache::MemoWrite {
        data: data.and_then(crate::provider::Value::from_json),
        hash: hash.to_string(),
        watch: watch.to_vec(),
        ttl_secs,
        log: log.to_vec(),
    };
    Ok((provider.to_string(), path, field, write))
}

/// Whether an object anywhere in `data` has a member named `_`.
fn names_underscore(data: &serde_json::Value) -> bool {
    match data {
        serde_json::Value::Object(map) => map
            .iter()
            .any(|(k, v)| libbeachcomber::key::field_error(k).is_some() || names_underscore(v)),
        serde_json::Value::Array(items) => items.iter().any(names_underscore),
        _ => false,
    }
}

/// A `put`: the request's handling, and a persisted put's replay at start
/// ([`crate::daemon`]), which goes the same way.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn put(
    cache: &Arc<Cache>,
    registry: &Arc<ProviderRegistry>,
    scheduler: &SchedulerHandle,
    key: &str,
    data: &serde_json::Value,
    ttl: Option<&str>,
    path: Option<&str>,
    persist: bool,
) -> Response {
    // A put key addresses a node: `namespace` or `namespace.node…`,
    // the node in canonical form.
    let (namespace, node) = match libbeachcomber::key::split_key(key) {
        Ok(split) => split,
        Err(msg) => return Response::error(msg),
    };
    if let Err(msg) = libbeachcomber::key::check_head(namespace) {
        return Response::error(msg);
    }
    let node = node.as_str();

    // The facet a put names is one of the six, and only three of them
    // are written to.
    use libbeachcomber::key::Facet;
    let facet = match libbeachcomber::key::split_facet(namespace) {
        Ok((base, Some(Facet::Put | Facet::Source))) => {
            return Response::error(format!(
                "{base}:put and {base}:source are read-only; write {base}.<node>"
            ));
        }
        Ok((base, Some(Facet::Memo))) => {
            return Response::error(format!(
                "{base}:memo is written by cache(); edit {base}:template.<f>, or refresh \
                 {base}"
            ));
        }
        Ok((_, facet)) => facet,
        Err(msg) => return Response::error(msg),
    };

    // The one `:cache` key a put may write is a decay prototype. The
    // rest is derived at a read or stored by the daemon.
    if facet == Some(Facet::Cache) {
        let derived = ["age_ms", "source", "executing"]
            .iter()
            .any(|k| libbeachcomber::key::last(node).as_deref() == Some(k));
        if derived {
            return Response::error(format!("{key} is derived and cannot be written"));
        }
        if node != "_.decay" {
            return Response::error(format!("{key} is written by the daemon"));
        }
        if !data.is_null()
            && !data
                .as_i64()
                .is_some_and(|n| (1..=crate::scheduler::lifecycle::MAX_DECAY).contains(&n))
        {
            return Response::error(crate::scheduler::lifecycle::decay_error(
                key,
                &data.to_string(),
            ));
        }
    }

    // `_` names no field, at the address or anywhere in what is written
    // there; in `:cache` it is the decay prototype.
    if facet != Some(Facet::Cache)
        && (libbeachcomber::key::segments(node)
            .iter()
            .any(|s| libbeachcomber::key::field_error(s).is_some())
            || names_underscore(data))
    {
        return Response::error(format!(
            "invalid field in {key}: {}",
            libbeachcomber::key::NOT_A_FIELD
        ));
    }

    // A template is linted as a configured one is: one that does not
    // compile is refused. A name no provider has is not, since a later
    // put may make that provider.
    if facet == Some(Facet::Template) {
        // A var is configuration only, for now: `[client.providers.<p>] var.<name>`.
        let var = crate::shape::seeded::VAR_NODE;
        let at_var = libbeachcomber::key::segments(node)
            .first()
            .map(|s| s.as_ref())
            == Some(var)
            || (node.is_empty() && data.get(var).is_some());
        if at_var {
            return Response::error(format!(
                "{key}: {var} is the vars' node, set in [client.providers.<p>] {var}.<name>; a \
                 put does not write a var"
            ));
        }
        let engine = libbeachcomber::eval::Probe::new(libbeachcomber::engine::engine_env());
        let at = libbeachcomber::key::join(namespace, node);
        if let Some(leaf) = crate::daemon::seed::non_string_template_leaf(data, &at) {
            // The CLI already prefixes the key it put to; name a leaf only
            // when it is below that key.
            let who = if leaf == at {
                String::new()
            } else {
                format!("{leaf} ")
            };
            return Response::error(format!("{who}must be a string (a value expression)"));
        }
        let known = libbeachcomber::eval::Known::default();
        let refused = crate::daemon::seed::lint_template_tree(&engine, data, &at, &known)
            .into_iter()
            .find(|(_, f)| matches!(f, libbeachcomber::eval::Finding::Compile(_)));
        if let Some((leaf, finding)) = refused {
            return Response::error(format!("{leaf}: {finding}"));
        }
    }
    // A macro library is checked as a configured one is: a string,
    // under a name, that compiles and prints nothing.
    if facet == Some(Facet::Macro)
        && let Err(msg) = crate::daemon::seed::check_macro_put(namespace, node, data)
    {
        return Response::error(msg);
    }

    // A key arrives off the socket with no length bound of its own, and
    // every segment of the node is one step of a tree walk. Cap the
    // node at the depth JSON conversion is already capped at, so a
    // pathological key cannot drive the daemon into a stack overflow
    // here or on the next read of what it wrote.
    if libbeachcomber::key::segments(node).len() > crate::provider::MAX_JSON_DEPTH {
        return Response::error(format!(
            "put key is nested too deeply (limit {} segments)",
            crate::provider::MAX_JSON_DEPTH
        ));
    }

    // Resolve optional path — canonicalize if provided. `$HOME`, like a
    // request carrying no path at all, is the global entry.
    let effective_path: Option<String> = match crate::query::Dir::requested(path, registry.home()) {
        Ok(p) => p.into_address(),
        Err(e) => return Response::error(e),
    };

    // A ttl is the put's lifecycle: valid for N, stale for N more, then
    // gone. It needs a value to expire.
    if ttl.is_some() && data.is_null() {
        return Response::error("a ttl on an absent value has no meaning");
    }
    let ttl_secs = match parse_ttl(ttl) {
        Ok(secs) => secs,
        Err(msg) => return Response::error(msg),
    };
    let put_written = || crate::scheduler::SchedulerMessage::PutWritten {
        provider: namespace.to_string(),
        path: effective_path.clone(),
        node: node.to_string(),
        ttl_secs,
    };

    // `data: null` means "clear the addressed node" — the row goes, the
    // registry entry stays.
    // The latest put to an address decides whether it is persisted: whatever
    // was persisted at or below it goes, and a persisted put takes its place.
    let address = libbeachcomber::key::join(namespace, node);
    let store = cache.put_store();
    if data.is_null() {
        cache.remove_put(namespace, effective_path.as_deref(), node);
        if let Some(store) = store {
            store.record(&address, effective_path.as_deref(), None);
        }
        scheduler.send(put_written()).await;
        return Response::miss();
    }

    // `MAX_JSON_DEPTH` is counted from the record, not from the
    // namespace: at the root each top-level value is converted on its
    // own (as every other provider ingestion path does), and below the
    // root from the addressed node. A deep node's total tree depth can
    // therefore reach twice the bound — the node is capped at
    // MAX_JSON_DEPTH segments above, the value at MAX_JSON_DEPTH below
    // — which is what keeps the recursion bounded either way.
    //
    // The root's object check runs on the wire JSON, before conversion:
    // `Value::from_json` turns an array into an index-keyed object, so
    // checking afterwards would let `put ns [1,2]` through.
    let value = match data.as_object() {
        Some(obj) if node.is_empty() => crate::provider::Value::Object(
            crate::provider::SourceResult::from_json_object(obj).fields,
        ),
        None if node.is_empty() => {
            return Response::error(crate::cache::PutError::RootNotObject.to_string());
        }
        _ => crate::provider::Value::from_json(data).expect("null data handled above"),
    };

    let persisted = (persist && store.is_some()).then(|| value.clone());
    if let Err(e) = cache.write_static(
        namespace,
        effective_path.as_deref(),
        &crate::cache::Origin::Put,
        node,
        value,
        ttl_secs,
    ) {
        return Response::error(e.to_string());
    }
    if let Some(store) = store {
        let kept = persist.then(|| crate::persist::Persisted {
            key: address.clone(),
            path: effective_path.clone(),
            data: data.clone(),
            expires: ttl_secs.map(|secs| crate::persist::now_ms() + secs * 1000),
            schema: crate::config::migrate::current(),
        });
        store.record(&address, effective_path.as_deref(), kept);
    }
    if let Some(value) = persisted {
        cache.mark_persisted(namespace, effective_path.as_deref(), node, &value);
    }
    scheduler.send(put_written()).await;

    // A namespace no provider declares becomes a static one; a provider
    // namespace that merely gained a static record does not
    // (`register_static` refuses a registered name).
    registry.register_static(namespace);

    Response::miss()
}

/// `memo`: store a rendered template's value under its hash, and tell the
/// scheduler what it watches.
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_memo(
    ctx: &Ctx,
    key: &str,
    path: Option<&str>,
    hash: &str,
    data: Option<&serde_json::Value>,
    watch: &[String],
    ttl: Option<&str>,
    log: &[libbeachcomber::protocol::LogLine],
) -> Response {
    let home = ctx.registry.current().home().clone();
    match memo_write(key, path, hash, data, watch, ttl, log, &home) {
        Ok((provider, path, field, write)) => {
            let written = crate::scheduler::SchedulerMessage::MemoWritten {
                provider: provider.clone(),
                path: path.clone(),
                node: field.clone(),
                ttl_secs: write.ttl_secs,
                watch: write.watch.clone(),
            };
            ctx.cache
                .write_memo(&provider, path.as_deref(), &field, write);
            ctx.scheduler.send(written).await;
            Response::miss()
        }
        Err(msg) => Response::error(msg),
    }
}
