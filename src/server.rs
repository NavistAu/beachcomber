use crate::cache::{Cache, MergedRead};
use crate::config::Config;
use crate::protocol::{self, IntrospectSubject, Request, Response};
use crate::provider::registry::ProviderRegistry;
use crate::provider::{InvalidationStrategy, SourceScope};
use crate::query::{KeyParse, resolve_path};
use crate::scheduler::{
    DemandInfo, LifecycleInfo, PollTimerInfo, SchedulerHandle, SchedulerMessage,
};
use crate::watcher_registry::WatcherRegistry;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tracing::{debug, info, warn};

pub struct Server {
    socket_path: PathBuf,
    cache: Arc<Cache>,
    registry: Arc<ProviderRegistry>,
    scheduler: Option<SchedulerHandle>,
    watchers: Arc<WatcherRegistry>,
    start_instant: Instant,
    requests_total: Arc<AtomicU64>,
    /// Open `watch` streams, one per client, counted while each is streaming.
    /// Not the watcher registry's entry count: one watch subscribes to every
    /// entry on its path's chain, so that would report a single watch at depth
    /// six as six.
    active_watches: Arc<AtomicUsize>,
    config: Arc<Config>,
    /// Reaper health, present only in the daemon path (canon invariant 13).
    /// None for embedded/test servers — introspect then omits the field.
    reaper: Option<Arc<crate::singleton::ReaperHealth>>,
}

impl Server {
    pub fn new(
        socket_path: PathBuf,
        cache: Arc<Cache>,
        registry: Arc<ProviderRegistry>,
        scheduler: Option<SchedulerHandle>,
        watchers: Arc<WatcherRegistry>,
    ) -> Self {
        Self {
            socket_path,
            cache,
            registry,
            scheduler,
            watchers,
            start_instant: Instant::now(),
            requests_total: Arc::new(AtomicU64::new(0)),
            active_watches: Arc::new(AtomicUsize::new(0)),
            config: Arc::new(Config::load()),
            reaper: None,
        }
    }

    pub fn new_with_config(
        socket_path: PathBuf,
        cache: Arc<Cache>,
        registry: Arc<ProviderRegistry>,
        scheduler: Option<SchedulerHandle>,
        watchers: Arc<WatcherRegistry>,
        config: Config,
    ) -> Self {
        Self {
            socket_path,
            cache,
            registry,
            scheduler,
            watchers,
            start_instant: Instant::now(),
            requests_total: Arc::new(AtomicU64::new(0)),
            active_watches: Arc::new(AtomicUsize::new(0)),
            config: Arc::new(config),
            reaper: None,
        }
    }

    /// Attach reaper health state for introspect surfacing (daemon path only).
    pub fn with_reaper_health(mut self, health: Arc<crate::singleton::ReaperHealth>) -> Self {
        self.reaper = Some(health);
        self
    }

    pub async fn run(&self) -> std::io::Result<()> {
        let listener = self.bind()?;
        self.serve(listener).await
    }

    /// Bind the socket and start listening. Once this returns, clients can
    /// connect — connections queue in the backlog until [`Server::serve`]
    /// accepts them — so a caller that binds first never has to guess when the
    /// socket is live.
    pub fn bind(&self) -> std::io::Result<UnixListener> {
        if let Some(parent) = self.socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Clean up stale socket file. If another daemon is actively listening,
        // the bind will fail with EADDRINUSE — that's correct behavior.
        if self.socket_path.exists() {
            // Check if something is actually listening
            if std::os::unix::net::UnixStream::connect(&self.socket_path).is_ok() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!(
                        "Another daemon is already listening on {:?}",
                        self.socket_path
                    ),
                ));
            }
            // Stale socket file — remove it
            let _ = std::fs::remove_file(&self.socket_path);
        }

        let listener = UnixListener::bind(&self.socket_path)?;
        info!("Listening on {:?}", self.socket_path);
        Ok(listener)
    }

    /// Accept loop over a listener from [`Server::bind`]. Individual accept
    /// errors are logged and the loop continues.
    pub async fn serve(&self, listener: UnixListener) -> std::io::Result<()> {
        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    let cache = Arc::clone(&self.cache);
                    let registry = Arc::clone(&self.registry);
                    let scheduler = self.scheduler.clone();
                    let watchers = self.watchers.clone();
                    let start_instant = self.start_instant;
                    let requests_total = Arc::clone(&self.requests_total);
                    let active_watches = Arc::clone(&self.active_watches);
                    let socket_path = self.socket_path.clone();
                    let config = Arc::clone(&self.config);
                    let reaper = self.reaper.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(
                            stream,
                            cache,
                            registry,
                            scheduler,
                            watchers,
                            start_instant,
                            requests_total,
                            active_watches,
                            socket_path,
                            config,
                            reaper,
                        )
                        .await
                        {
                            debug!("Connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    warn!("Accept error: {}", e);
                }
            }
        }
    }

    /// Bind, then run the accept loop on its own task. The socket is
    /// connectable when this returns, so a caller can connect immediately
    /// rather than sleeping and hoping.
    pub fn spawn(self) -> std::io::Result<tokio::task::JoinHandle<std::io::Result<()>>> {
        let listener = self.bind()?;
        Ok(tokio::spawn(async move { self.serve(listener).await }))
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_connection(
    stream: tokio::net::UnixStream,
    cache: Arc<Cache>,
    registry: Arc<ProviderRegistry>,
    scheduler: Option<SchedulerHandle>,
    watchers: Arc<WatcherRegistry>,
    start_instant: Instant,
    requests_total: Arc<AtomicU64>,
    active_watches: Arc<AtomicUsize>,
    socket_path: PathBuf,
    config: Arc<Config>,
    reaper: Option<Arc<crate::singleton::ReaperHealth>>,
) -> std::io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    let mut context_path: Option<String> = None;

    while reader.read_line(&mut line).await? > 0 {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            line.clear();
            continue;
        }

        match serde_json::from_str::<Request>(trimmed) {
            Ok(Request::Watch { key, path }) => {
                requests_total.fetch_add(1, Ordering::Relaxed);
                // Watch takes over the connection — enter streaming mode
                let _open = WatchGuard::new(Arc::clone(&active_watches));
                handle_watch(
                    key,
                    path,
                    &context_path,
                    &cache,
                    &registry,
                    scheduler.as_ref(),
                    &watchers,
                    &mut writer,
                )
                .await;
                return Ok(());
            }
            Ok(request) => {
                requests_total.fetch_add(1, Ordering::Relaxed);
                let response = handle_request(
                    &request,
                    &cache,
                    &registry,
                    scheduler.as_ref(),
                    &mut context_path,
                    start_instant,
                    &requests_total,
                    &active_watches,
                    &socket_path,
                    &config,
                    reaper.as_deref(),
                )
                .await;
                let mut response_bytes = serde_json::to_string(&response).unwrap();
                response_bytes.push('\n');
                writer.write_all(response_bytes.as_bytes()).await?;
            }
            Err(e) => {
                let resp = Response::error(format!("invalid request: {e}"));
                let mut out = serde_json::to_string(&resp).unwrap();
                out.push('\n');
                writer.write_all(out.as_bytes()).await?;
            }
        };
        line.clear();
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_watch(
    key: String,
    path: Option<String>,
    context_path: &Option<String>,
    cache: &Cache,
    registry: &ProviderRegistry,
    scheduler: Option<&SchedulerHandle>,
    watchers: &Arc<WatcherRegistry>,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
) {
    let requested = path.as_deref().or(context_path.as_deref());
    let plan = crate::query::QueryPlan::build(&key, requested, registry);
    let provider_name = plan.provider.clone();

    // Facet syntax is checked on the provider segment, after the metadata-suffix
    // stripping QueryPlan::build already did (mirrors the Get path).
    if let Err(msg) = crate::query::parse_facet(&provider_name) {
        let _ = write_watch_line(writer, &Response::error(msg)).await;
        return;
    }

    let effective_path = plan.effective_path.clone();
    // The template half reads from the directory the caller named, not the
    // scope-resolved one — see [`read_watch_value`].
    let template_path = crate::query::normalise_path(requested);

    // Signal demand (source-aware: canon §150/§268).
    if let Some(sched) = scheduler {
        sched
            .send(SchedulerMessage::QueryActivity {
                provider: provider_name.clone(),
                path: effective_path.clone(),
                demand: plan.demand.clone(),
            })
            .await;
    }

    // Subscribe to every entry on the chain the read walks, not just the
    // addressed one: a `put` at an ancestor (or at the global entry) changes
    // what this watch resolves to, so it has to wake the stream. One forwarder
    // task per link funnels the per-key broadcasts into a single channel.
    let chain_watch = {
        let changed = Arc::new(tokio::sync::Notify::new());
        let mut tasks = Vec::new();
        for layer in crate::query::path_chain(effective_path.as_deref()) {
            let mut sub = watchers.subscribe(provider_name.as_str(), layer.as_deref());
            let changed = Arc::clone(&changed);
            tasks.push(tokio::spawn(async move {
                // A lagged subscriber has missed notifications, but one
                // "something changed" is all the stream loop needs — which is
                // also why a burst across several links collapses into one
                // wakeup rather than one re-read per notification.
                while let Ok(()) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) =
                    sub.recv().await
                {
                    changed.notify_one();
                }
            }));
        }
        ChainWatch { tasks, changed }
    };

    // Re-execute any read-always source(s) for the initial snapshot so the
    // first value is fresh off disk, not stale cache.
    match &plan.target {
        crate::query::KeyParse::Field(provider, field) => {
            let head = field.split('.').next().unwrap_or(field.as_str());
            if let Some(source_name) = registry.source_for_field(provider, head) {
                maybe_read_always(
                    registry,
                    cache,
                    provider,
                    source_name,
                    effective_path.as_deref(),
                )
                .await;
            }
        }
        crate::query::KeyParse::Provider(provider) => {
            if let Some(sources) = registry.provider_sources(provider) {
                // Every source applies, whatever the address: a path-scoped one
                // at the global entry runs at `$HOME` like any other directory
                // (`query::source_dir`).
                let source_names: Vec<String> = sources.iter().map(|sm| sm.name.clone()).collect();
                for sn in &source_names {
                    maybe_read_always(registry, cache, provider, sn, effective_path.as_deref())
                        .await;
                }
            }
        }
    }

    // Send initial value
    let initial = read_watch_value(
        cache,
        registry,
        &plan.target,
        effective_path.as_deref(),
        template_path.as_deref(),
    );
    if write_watch_line(writer, &initial).await.is_err() {
        return;
    }

    let mut last_data = initial.data.clone();

    // Stream loop
    loop {
        chain_watch.changed.notified().await;
        // Signal ongoing demand
        if let Some(sched) = scheduler {
            sched
                .send(SchedulerMessage::QueryActivity {
                    provider: provider_name.clone(),
                    path: effective_path.clone(),
                    demand: plan.demand.clone(),
                })
                .await;
        }

        let response = read_watch_value(
            cache,
            registry,
            &plan.target,
            effective_path.as_deref(),
            template_path.as_deref(),
        );

        // Field-level filtering: skip if value unchanged
        if response.data == last_data {
            continue;
        }
        last_data = response.data.clone();

        if write_watch_line(writer, &response).await.is_err() {
            break; // Client disconnected
        }
    }
}

/// Counts one open `watch` for as long as its stream is live.
struct WatchGuard(Arc<AtomicUsize>);

impl WatchGuard {
    fn new(count: Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::Relaxed);
        Self(count)
    }
}

impl Drop for WatchGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The chain subscription a `watch` holds: one forwarder task per entry on the
/// chain, funnelling into `rx`. Dropping it stops the forwarders.
struct ChainWatch {
    tasks: Vec<tokio::task::JoinHandle<()>>,
    changed: Arc<tokio::sync::Notify>,
}

impl Drop for ChainWatch {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Cold-miss inline-execute: synchronously run a Source to populate the cache,
/// then let the caller re-read. Mirrors the scheduler's execute_source path
/// (spawn_blocking + cache.put_source) but runs in the request task so the
/// response carries fresh data (canon §"Cold cache miss triggers inline fetch").
///
/// Path argument is the consumer-supplied path; for Global sources it is
/// ignored and the cache write keys to (provider, None). Returns true if a
/// non-empty result was written.
///
/// The cache *address* and the *directory* the Source runs in are two things:
/// the global entry's address is `None` and its directory is `$HOME`
/// (`query::normalise_path`), so a PathScoped Source addressed there runs at
/// `$HOME` and its records land on the pathless entry.
async fn inline_execute_source(
    registry: &ProviderRegistry,
    cache: &Cache,
    provider: &str,
    source_name: &str,
    path: Option<&str>,
) -> bool {
    let Some(source) = registry.source(provider, source_name) else {
        return false;
    };
    let scope = source.metadata().scope;
    let address = match scope {
        SourceScope::Global => None,
        SourceScope::PathScoped => path.map(|s| s.to_string()),
    };
    let expected_interval_secs = match source.metadata().invalidation {
        InvalidationStrategy::Poll { interval_secs } => Some(interval_secs),
        InvalidationStrategy::WatchAndPoll { interval_secs, .. } => Some(interval_secs),
        InvalidationStrategy::Watch { .. } => None,
    };
    let dir = crate::query::source_dir(scope, address.as_deref());
    let src_clone = std::sync::Arc::clone(&source);
    let result = match tokio::task::spawn_blocking(move || src_clone.execute(dir.as_deref())).await
    {
        Ok(r) => r,
        Err(_) => return false,
    };
    if result.fields.is_empty() {
        return false;
    }
    cache.put_source(
        provider,
        address.as_deref(),
        source_name,
        result.fields,
        expected_interval_secs,
    );
    true
}

/// Bring one Source up to date for a read addressed at `at`, and say whether it
/// ran.
///
/// The decision is per Source, never per provider: a warm sibling says nothing
/// about this one, so a cold Source is executed even when the namespace already
/// holds records. It runs when it is `read_always`, when `force` asks for it, or
/// when it has nothing cached at its own scope's entry — the entry
/// [`source_scope_path`] names, which for a `Global` Source is the global one
/// whatever path the request carried.
///
/// `force` drops that Source's map first, so `--force` refreshes every record
/// the read returns without evicting a whole entry: the static records sharing
/// the entry survive, and a `Global` Source is refreshed where it actually
/// lives rather than being missed because the request named a directory.
async fn warm_source(
    registry: &ProviderRegistry,
    cache: &Cache,
    provider: &str,
    source_name: &str,
    at: Option<&str>,
    force: bool,
) -> bool {
    let Some(source) = registry.source(provider, source_name) else {
        return false;
    };
    let layer = source_scope_path(registry, provider, source_name, at);
    if force {
        cache.remove_source(provider, layer, source_name);
    } else if !source.read_always() && cache.has_provider_records_for(provider, layer, source_name)
    {
        return false;
    }
    inline_execute_source(registry, cache, provider, source_name, at).await;
    true
}

/// Re-execute a source inline if it is `read_always`, refreshing the cache
/// before the caller reads from it. Reuses `inline_execute_source`. No-op if
/// the source is not registered or does not have `read_always() == true`.
///
/// `watch`'s half of [`warm_source`]: a watch never executes on a cold miss —
/// the scheduler will populate the entry and the stream carries the value then
/// — so only the read-always case applies to it.
async fn maybe_read_always(
    registry: &ProviderRegistry,
    cache: &Cache,
    provider: &str,
    source_name: &str,
    path: Option<&str>,
) {
    if let Some(src) = registry.source(provider, source_name)
        && src.read_always()
    {
        inline_execute_source(registry, cache, provider, source_name, path).await;
    }
}

/// The value at `field` in a merged read, as wire JSON.
fn lookup_merged(merged: &Option<MergedRead>, field: &str) -> Option<serde_json::Value> {
    let value = crate::provider::lookup_path(&merged.as_ref()?.fields, field)?;
    Some(serde_json::to_value(value).unwrap_or(serde_json::Value::Null))
}

/// A data hit, carrying the freshness [`freshness`] found for it.
///
/// Every node of a merged tree has at least one record at or below it, so a hit
/// with no freshness is a bug in the merge rather than a value to report. The
/// fields are omitted rather than filled in with a zero that would read as
/// "just refreshed".
fn hit_response(data: serde_json::Value, freshness: Option<(u128, bool)>) -> Response {
    debug_assert!(
        freshness.is_some(),
        "a data hit with no record behind it: the merge lost provenance"
    );
    match freshness {
        Some((age_ms, stale)) => Response::ok(data, age_ms, stale),
        None => Response {
            data: Some(data),
            ..Response::miss()
        },
    }
}

/// The `age_ms`/`stale` pair to report for the node at `rest` (`""` for the
/// whole namespace) in a merged read.
///
/// A leaf is answered by its own record's source entry — canon
/// `provider_source.md` §"Field freshness". An object is answered by the
/// records under it together: the largest of their ages, and stale if any one
/// of them is. `None` when no record sits at or below the address, which is
/// also when there is no `data` to report freshness for.
fn freshness(read: &MergedRead, rest: &str) -> Option<(u128, bool)> {
    if let Some(record) = read.records.get(rest) {
        return Some((record.age_ms(), record.is_stale()));
    }
    let below = if rest.is_empty() {
        String::new()
    } else {
        format!("{rest}.")
    };
    let mut oldest: Option<u128> = None;
    let mut stale = false;
    for (tail, record) in &read.records {
        if !tail.starts_with(&below) {
            continue;
        }
        let age = record.age_ms();
        oldest = Some(oldest.map_or(age, |o| o.max(age)));
        stale |= record.is_stale();
    }
    oldest.map(|age| (age, stale))
}

/// The template half of a `get` (spec §3.4): the node at `rest` in the merged
/// tree of `provider:template` at the same address, and the part of `rest` the
/// walk could not consume.
///
/// A group (object) or a value expression (string) at the address is the
/// template. A string reached with segments still to go is the template too,
/// and the remainder comes back as `rest` for the client to apply to whatever
/// the expression renders. Anything else — a miss, or a non-string scalar in
/// the way — attaches nothing.
fn template_half(
    cache: &Cache,
    registry: &ProviderRegistry,
    provider: &str,
    rest: &str,
    path: Option<&str>,
) -> (Option<serde_json::Value>, Option<String>) {
    use crate::provider::Value;
    let none = (None, None);
    let Some(read) = cache.read_merged(&format!("{provider}:template"), path, registry) else {
        return none;
    };

    if rest.is_empty() {
        return (serde_json::to_value(&read.fields).ok(), None);
    }

    let mut segments = rest.split('.');
    let Some(mut current) = read
        .fields
        .get(segments.next().expect("split is never empty"))
    else {
        return none;
    };
    while let Some(segment) = segments.next() {
        match current {
            Value::Object(map) => match map.get(segment) {
                Some(v) => current = v,
                None => return none,
            },
            // A value expression with the address unconsumed: the client
            // renders it and walks the remainder into the result.
            Value::String(_) => {
                let left: Vec<&str> = std::iter::once(segment).chain(segments.by_ref()).collect();
                return (serde_json::to_value(current).ok(), Some(left.join(".")));
            }
            _ => return none,
        }
    }
    match current {
        Value::Object(_) | Value::String(_) => (serde_json::to_value(current).ok(), None),
        _ => none,
    }
}

/// The cache-entry address a Source's own records live at: the global entry for
/// a `Global` source whatever path the request carried, the addressed path for
/// a `PathScoped` one.
fn source_scope_path<'a>(
    registry: &ProviderRegistry,
    provider: &str,
    source_name: &str,
    path: Option<&'a str>,
) -> Option<&'a str> {
    let scope = registry
        .provider_sources(provider)
        .and_then(|ss| ss.iter().find(|sm| sm.name == source_name))
        .map(|sm| sm.scope);
    match scope {
        Some(SourceScope::Global) => None,
        _ => path,
    }
}

// Watch reads only from cache — no inline-execute on cold miss (unlike Get).
// A cache miss in a streaming context means the scheduler has not populated the
// entry yet; the caller receives an update when it does. Routing by KeyParse
// here mirrors Get's cache routing exactly so `watch X` resolves identically to
// `get X` (the core parity goal of this change) — including the chain-resolved
// merged read.
fn read_watch_value(
    cache: &Cache,
    registry: &ProviderRegistry,
    target: &KeyParse,
    path: Option<&str>,
    template_path: Option<&str>,
) -> Response {
    // `rest` is the address below the namespace — empty for a whole-namespace
    // read, which is also how both halves address the root.
    let (provider, rest) = match target {
        KeyParse::Field(p, f) => (p.as_str(), f.as_str()),
        KeyParse::Provider(p) => (p.as_str(), ""),
    };
    // A watch frame carries the same two halves a `get` does, so the client
    // renders a streamed value exactly as it renders a fetched one. A facet key
    // is already inside a facet and never gets one. Read first: the template
    // decides whether an unresolvable sub-path is an error or a hit.
    let (template, template_rest) = if crate::query::parse_facet(provider).ok().flatten().is_some()
    {
        (None, None)
    } else {
        template_half(cache, registry, provider, rest, template_path)
    };
    let merged = cache.read_merged(provider, path, registry);
    let response = match target {
        KeyParse::Field(..) => match (lookup_merged(&merged, rest), &merged) {
            (Some(data), Some(m)) => hit_response(data, freshness(m, rest)),
            // Mirror Get's Field-miss semantics exactly: a failed nested
            // sub-path (e.g. `p.project.nonesuch` where `project` exists but
            // `nonesuch` doesn't) is a loud "unknown field" error; anything
            // else is a plain miss.
            _ => {
                let nested_head_exists = rest.contains('.')
                    && rest
                        .split('.')
                        .next()
                        .is_some_and(|head| lookup_merged(&merged, head).is_some());
                if template.is_none() && nested_head_exists {
                    return Response::error(format!("unknown field: {provider}.{rest}"));
                }
                Response::miss()
            }
        },
        KeyParse::Provider(_) => match &merged {
            Some(m) => hit_response(
                serde_json::to_value(&m.fields).unwrap_or(serde_json::Value::Null),
                freshness(m, ""),
            ),
            None => Response::miss(),
        },
    };
    response.with_template(template, template_rest)
}

async fn write_watch_line(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    response: &Response,
) -> Result<(), std::io::Error> {
    let mut line = serde_json::to_string(response).unwrap();
    line.push('\n');
    writer.write_all(line.as_bytes()).await
}

#[allow(clippy::too_many_arguments)]
async fn handle_request(
    request: &Request,
    cache: &Cache,
    registry: &ProviderRegistry,
    scheduler: Option<&SchedulerHandle>,
    context_path: &mut Option<String>,
    start_instant: Instant,
    requests_total: &AtomicU64,
    active_watches: &AtomicUsize,
    socket_path: &std::path::Path,
    config: &Config,
    reaper: Option<&crate::singleton::ReaperHealth>,
) -> Response {
    match request {
        Request::Get {
            key,
            path,
            force,
            wait,
            ..
        } => {
            // One source-aware plan drives the whole request: provider, resolved
            // path, parsed target, metadata suffix, and the warming demand. Get
            // and Watch both build this so they share identical key semantics.
            let requested = path.as_deref().or(context_path.as_deref());
            let plan = crate::query::QueryPlan::build(key, requested, registry);
            let provider_name = plan.provider.as_str();

            // Facet syntax is checked on the provider segment produced by the
            // plan — after metadata-suffix stripping, so e.g. `a:age` is read
            // as provider `a` with the `:age` suffix, not as a facet.
            let facet = match crate::query::parse_facet(provider_name) {
                Ok(facet) => facet,
                Err(msg) => return Response::error(msg),
            };

            let effective_path = plan.effective_path.clone();
            let meta = plan.meta.as_deref();

            // The two halves are addressed from different places. `effective_path`
            // is where the *provider's* records live: `resolve_path` has already
            // collapsed it to the global entry for a provider with only `Global`
            // sources, and walked it to the repository root for git. A template is
            // a static record, and static records resolve up the chain from the
            // directory the caller is standing in — so the template half reads from
            // the requested path, canonicalised, and nothing else.
            let template_path = crate::query::normalise_path(requested);

            // Unknown provider check. A namespace is known when a provider
            // declares it, when a `put` registered it as static, or when a
            // record for it sits anywhere on the chain — including one under
            // its `template` facet. A namespace that holds only templates
            // (`scanline`, or an `aws` whose daemon provider was retired) still
            // answers a `get` with its template half. A facet key is already
            // inside a facet and has no template half to look for.
            let is_known = registry.provider_metadata(provider_name).is_some()
                || registry.is_static_namespace(provider_name)
                || cache.has_records_on_chain(provider_name, effective_path.as_deref())
                || (facet.is_none()
                    && cache.has_records_on_chain(
                        &format!("{provider_name}:template"),
                        template_path.as_deref(),
                    ));
            if !is_known {
                return Response::error(format!("unknown provider: {provider_name}"));
            }

            // `--force` re-executes below, per source and at each source's own
            // scope; nothing is evicted here. Evicting the addressed entry
            // wholesale used to take the static records with it and left a
            // Global source's records — which live at another entry — untouched
            // and unrefreshed.
            if *force && registry.is_static_namespace(provider_name) {
                return Response::error(format!(
                    "cannot --force static provider '{provider_name}': no source to re-execute from"
                ));
            }

            // Wait semantics: if the cached entry is stale, evict it so the normal
            // miss path below re-executes the provider inline and returns fresh data.
            // Skipped for static namespaces — they have no source to re-execute.
            if *wait
                && !*force
                && !registry.is_static_namespace(provider_name)
                && cache
                    .get_entry(provider_name, effective_path.as_deref())
                    .is_some_and(|e| e.is_stale())
            {
                cache.remove(provider_name, effective_path.as_deref());
                // Fall through to the normal miss path, which executes inline.
            }

            // Signal demand to scheduler — keeps only the queried Source(s) warm
            // (canon §150/§268: a field query demands only its owning Source).
            if let Some(sched) = scheduler {
                sched
                    .send(SchedulerMessage::QueryActivity {
                        provider: provider_name.to_string(),
                        path: effective_path.clone(),
                        demand: plan.demand.clone(),
                    })
                    .await;
            }

            // :source short-circuits — no cache lookup needed.
            if matches!(meta, Some("source")) {
                let src = if registry.is_static_namespace(provider_name) {
                    "static"
                } else if registry.provider_metadata(provider_name).is_some() {
                    "builtin"
                } else {
                    "unknown"
                };
                return Response::ok(serde_json::Value::String(src.to_string()), 0, false);
            }

            // Cache lookup with cold-miss inline-execute, routed by key parse form.
            // Every read goes through `Cache::read_merged`, which resolves the
            // directory chain (canon §"Path resolution"); there is no slot to
            // prefer and none to fall back to.
            // Canon §"Cold cache miss triggers inline fetch": on cache miss, synchronously
            // execute the relevant source(s), write to cache, then re-read.
            // Read-always sources are re-executed on every read, not just cold misses.
            //
            // `at` is the entry the request addresses and the head of the chain
            // the merged read walks.
            let at = effective_path.as_deref();

            // The template half is read before the data half, because it decides
            // what an unresolvable sub-path means: `get box.widgets.foo` where
            // `box.widgets` is an object without a `foo` is only "unknown field"
            // when no template answers there either.
            let rest = match &plan.target {
                KeyParse::Field(_, field) => field.as_str(),
                KeyParse::Provider(_) => "",
            };
            let (template, template_rest) = if facet.is_some() {
                (None, None)
            } else {
                template_half(
                    cache,
                    registry,
                    provider_name,
                    rest,
                    template_path.as_deref(),
                )
            };

            let (cache_hit, normal_response) = match &plan.target {
                KeyParse::Field(provider, field) => {
                    let head = field.split('.').next().unwrap_or(field.as_str());
                    let owner = registry.source_for_field(provider, head);
                    let ran = match owner {
                        Some(source_name) => {
                            warm_source(registry, cache, provider, source_name, at, *force).await
                        }
                        None => false,
                    };
                    // A source whose map exists without holding this address is
                    // still worth one attempt — `warm_source` only knows whether
                    // the source has *anything* cached.
                    let mut merged = cache.read_merged(provider, at, registry);
                    let mut hit = lookup_merged(&merged, field);
                    if hit.is_none()
                        && !ran
                        && let Some(source_name) = owner
                        && inline_execute_source(registry, cache, provider, source_name, at).await
                    {
                        merged = cache.read_merged(provider, at, registry);
                        hit = lookup_merged(&merged, field);
                    }
                    match (hit, &merged) {
                        (Some(data), Some(m)) => (true, hit_response(data, freshness(m, field))),
                        _ => {
                            // Distinguish nested-path not-found from full miss —
                            // unless a template answers the address, in which
                            // case the key is not unknown at all.
                            if template.is_none()
                                && field.contains('.')
                                && lookup_merged(&merged, head).is_some()
                            {
                                return Response::error(format!(
                                    "unknown field: {provider}.{field}"
                                ));
                            }
                            (false, Response::miss())
                        }
                    }
                }
                KeyParse::Provider(provider) => {
                    // Whole-namespace read: every source of the provider is
                    // brought up to date on its own terms — one cold source is
                    // no longer covered for by a warm sibling. Every source
                    // applies, whatever the address: a path-scoped one at the
                    // global entry runs at `$HOME` like any other directory
                    // (`query::source_dir`), so a repository rooted there
                    // answers a bare `--global` read.
                    let source_names: Vec<String> = registry
                        .provider_sources(provider)
                        .map(|sources| sources.iter().map(|sm| sm.name.clone()).collect())
                        .unwrap_or_default();
                    for sn in &source_names {
                        warm_source(registry, cache, provider, sn, at, *force).await;
                    }
                    match cache.read_merged(provider, at, registry) {
                        Some(m) => {
                            let data =
                                serde_json::to_value(&m.fields).unwrap_or(serde_json::Value::Null);
                            (true, hit_response(data, freshness(&m, "")))
                        }
                        None => (false, Response::miss()),
                    }
                }
            };

            match meta {
                // Spec §3.4: a plain `get` carries both halves. A metadata
                // suffix asks one question about the data half and is answered
                // with that alone.
                None => normal_response.with_template(template, template_rest),
                Some("age") => Response::ok(
                    normal_response
                        .age_ms
                        .map(|n| {
                            // age_ms is u128 but realistic daemon ages fit in u64 (~585M years).
                            serde_json::Value::Number(serde_json::Number::from(n as u64))
                        })
                        .unwrap_or(serde_json::Value::Null),
                    0,
                    false,
                ),
                Some("stale") => Response::ok(
                    normal_response
                        .stale
                        .map(serde_json::Value::Bool)
                        .unwrap_or(serde_json::Value::Null),
                    0,
                    false,
                ),
                Some("fresh") => Response::ok(
                    normal_response
                        .stale
                        .map(|s| serde_json::Value::Bool(!s))
                        .unwrap_or(serde_json::Value::Null),
                    0,
                    false,
                ),
                Some("cache") => Response::ok(serde_json::Value::Bool(cache_hit), 0, false),
                Some(other) => Response::error(format!("unknown metadata suffix: :{other}")),
            }
        }
        Request::Refresh { key, path } => {
            // Suffix-strip runs before the facet check, mirroring Get/Watch, so
            // e.g. `refresh hostname:age` treats `age` as a metadata suffix on
            // provider `hostname`, not a facet candidate.
            let (stripped_key, _meta) = crate::query::split_metadata_suffix(key);
            let (provider_name, _field) = protocol::split_key(stripped_key);
            if let Err(msg) = crate::query::parse_facet(provider_name) {
                return Response::error(msg);
            }
            let requested = path.as_deref().or(context_path.as_deref());
            let effective_path = resolve_path(stripped_key, requested, registry);

            if let Some(sched) = scheduler {
                // Route through scheduler.
                sched
                    .send(SchedulerMessage::Refresh {
                        provider: provider_name.to_string(),
                        path: effective_path,
                    })
                    .await;
                Response {
                    ok: true,
                    data: None,
                    age_ms: None,
                    stale: None,
                    error: None,
                    template: None,
                    rest: None,
                }
            } else {
                // Fallback: scheduler not available. Static namespaces are a no-op.
                if registry.is_static_namespace(provider_name) {
                    return Response {
                        ok: true,
                        data: None,
                        age_ms: None,
                        stale: None,
                        error: None,
                        template: None,
                        rest: None,
                    };
                }
                // No providers are registered yet (Section H); return ok anyway
                // so callers don't see spurious errors during the build-up phase.
                if registry.provider_metadata(provider_name).is_none() {
                    return Response::error(format!("unknown provider: {provider_name}"));
                }
                Response {
                    ok: true,
                    data: None,
                    age_ms: None,
                    stale: None,
                    error: None,
                    template: None,
                    rest: None,
                }
            }
        }
        Request::Context { path } => {
            *context_path = Some(path.clone());
            Response {
                ok: true,
                data: None,
                age_ms: None,
                stale: None,
                error: None,
                template: None,
                rest: None,
            }
        }
        Request::Put {
            key,
            data,
            ttl,
            path,
            replace,
        } => {
            // A put key addresses a node: `namespace` or `namespace.node…`.
            let (namespace, node) = protocol::split_key(key);
            let node = node.unwrap_or("");

            // Facet syntax is checked on the namespace segment — `put` has no
            // metadata-suffix handling, so e.g. `a:age` is always a facet
            // candidate here (and rejected, since `age` is a metadata suffix
            // name).
            if let Err(msg) = crate::query::parse_facet(namespace) {
                return Response::error(msg);
            }

            // A key arrives off the socket with no length bound of its own, and
            // every segment of the node is one step of a tree walk. Cap the
            // node at the depth JSON conversion is already capped at, so a
            // pathological key cannot drive the daemon into a stack overflow
            // here or on the next read of what it wrote.
            if node.split('.').count() > crate::provider::MAX_JSON_DEPTH {
                return Response::error(format!(
                    "put key is nested too deeply (limit {} segments)",
                    crate::provider::MAX_JSON_DEPTH
                ));
            }

            // Resolve optional path — canonicalize if provided. `$HOME`, like a
            // request carrying no path at all, is the global entry.
            let effective_path: Option<String> = crate::query::normalise_path(path.as_deref());

            // data=None means "clear the addressed node" — the row goes, the
            // registry entry stays.
            let Some(data) = data else {
                return match cache.remove_static_path(
                    namespace,
                    effective_path.as_deref(),
                    node,
                    registry,
                ) {
                    Ok(_) => Response {
                        ok: true,
                        data: None,
                        age_ms: None,
                        stale: None,
                        error: None,
                        template: None,
                        rest: None,
                    },
                    Err(e) => Response::error(e.to_string()),
                };
            };

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
                _ => crate::provider::Value::from_json(data),
            };

            let interval_secs = ttl
                .as_deref()
                .and_then(crate::scheduler::parse_duration_secs_pub);

            if let Err(e) = cache.write_static(
                namespace,
                effective_path.as_deref(),
                crate::cache::PUT_SOURCE,
                node,
                value,
                *replace,
                interval_secs,
                registry,
            ) {
                return Response::error(e.to_string());
            }

            // A namespace no provider declares becomes a static one; a provider
            // namespace that merely gained a static record does not
            // (`register_static` refuses a registered name).
            registry.register_static(namespace);

            Response {
                ok: true,
                data: None,
                age_ms: None,
                stale: None,
                error: None,
                template: None,
                rest: None,
            }
        }
        Request::Status => {
            let mut rows = cache.list_rows();

            let (lifecycle, failures) = if let Some(sched) = scheduler {
                let lc = sched.get_lifecycle_snapshots().await;
                let fs = sched.get_failure_states().await;
                (lc, fs)
            } else {
                (Default::default(), Default::default())
            };

            use crate::cache::RowKind;
            for row in rows.iter_mut() {
                // `Cache::list_rows` already marked static rows (source ==
                // PUT_SOURCE or "config:*") with RowKind::Static; only rows
                // backed by a real Source need a lifecycle/transient lookup.
                let is_static = row.kind == Some(RowKind::Static);

                if !is_static {
                    // Look up the lifecycle snapshot for THIS row's owning source
                    // so glyphs and TTL columns reflect the source's strategy
                    // (refs vs diff vs status all differ within a single provider).
                    let triple = (row.provider.clone(), row.path.clone(), row.source.clone());
                    let matching_snap = lifecycle.get(&triple);

                    if let Some(snap) = matching_snap {
                        row.kind = Some(RowKind::Lifecycle {
                            decay: snap.decay,
                            watches_files: snap.watches_files,
                        });
                        // Only expose poll-related metadata when this source has
                        // a poll path. Pure Watch sources leave these None so the
                        // renderer doesn't fabricate `0s×00`.
                        if snap.poll_interval_secs > 0 {
                            row.poll_interval_secs = Some(snap.poll_interval_secs);
                            row.keep_alive_polls = Some(snap.keep_alive_polls);
                            row.polls_elapsed = Some(snap.polls_elapsed);
                            row.next_poll_in_secs = snap.next_poll_in_secs;
                        }
                        row.fsevents_reinstate = Some(snap.fsevents_reinstate);
                    } else {
                        row.kind = Some(RowKind::Transient);
                    }

                    // Failure backoff: per-source.
                    if let Some(snap) = failures.get(&triple) {
                        row.failure = Some(snap.clone());
                    }
                }
            }

            match serde_json::to_value(&rows) {
                Ok(v) => Response::ok(v, 0, false),
                Err(e) => Response::error(format!("serialization failed: {e}")),
            }
        }
        Request::Introspect {
            subject,
            duration_secs,
        } => match subject {
            IntrospectSubject::Daemon => {
                // Gather in_flight + watch backend from scheduler status if available.
                let (in_flight_count, watch_backend) = if let Some(sched) = scheduler
                    && let Some(sched_status) = sched.get_status().await
                {
                    (
                        sched_status.in_flight.len() as u64,
                        sched_status.watch_backend,
                    )
                } else {
                    (0, "unknown".to_string())
                };
                let active_watchers = active_watches.load(Ordering::Relaxed) as u64;
                let cache_entries = cache.len() as u64;
                handle_introspect_daemon(
                    socket_path,
                    start_instant,
                    requests_total,
                    in_flight_count,
                    active_watchers,
                    cache_entries,
                    &watch_backend,
                    reaper,
                )
            }
            IntrospectSubject::Providers => handle_introspect_providers(registry, scheduler).await,
            IntrospectSubject::Config => handle_introspect_config(config),
            IntrospectSubject::Cache => handle_introspect_cache(cache),
            IntrospectSubject::Lifecycle => handle_introspect_lifecycle(scheduler).await,
            IntrospectSubject::Watches => handle_introspect_watches(scheduler).await,
            IntrospectSubject::Timers => handle_introspect_timers(scheduler).await,
            IntrospectSubject::Demand => handle_introspect_demand(scheduler).await,
            IntrospectSubject::Procs => {
                let dur = *duration_secs;
                tokio::task::spawn_blocking(move || handle_introspect_procs(dur))
                    .await
                    .unwrap_or_else(|e| Response::error(format!("procs task panicked: {e}")))
            }
        },
        Request::Hello => {
            let data = serde_json::json!({
                "protocol_version": crate::protocol::PROTOCOL_VERSION,
                "daemon_version": env!("BEACHCOMBER_VERSION"),
            });
            Response::ok(data, 0, false)
        }
        // Watch is intercepted in handle_connection before reaching here
        Request::Watch { .. } => unreachable!("Watch handled before handle_request"),
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_introspect_daemon(
    socket_path: &std::path::Path,
    start_instant: Instant,
    requests_total: &AtomicU64,
    in_flight_count: u64,
    active_watchers: u64,
    cache_entries: u64,
    watch_backend: &str,
    reaper: Option<&crate::singleton::ReaperHealth>,
) -> Response {
    let config_path = crate::config::Config::config_path_if_exists()
        .map(|p| serde_json::Value::String(p.to_string_lossy().into_owned()))
        .unwrap_or(serde_json::Value::Null);

    let uptime_secs = start_instant.elapsed().as_secs();

    let mut verdicts = vec![serde_json::json!({"level": "PASS", "message": "daemon responsive"})];

    if in_flight_count > 50 {
        verdicts.push(serde_json::json!({
            "level": "WARN",
            "message": format!("{in_flight_count} in-flight requests (threshold 50)")
        }));
    } else {
        verdicts.push(serde_json::json!({
            "level": "PASS",
            "message": format!("in_flight={in_flight_count}")
        }));
    }

    // Canon provider_source.md invariant 16: watch degradation is observable.
    if watch_backend == "native" {
        verdicts.push(serde_json::json!({
            "level": "PASS",
            "message": "watch backend: native fs events"
        }));
    } else {
        verdicts.push(serde_json::json!({
            "level": "WARN",
            "message": format!(
                "watch backend: {watch_backend} — kernel fs events undelivered; watch invalidation degraded to polling"
            )
        }));
    }

    // Canon singleton.md invariant 13: reaper capability is never silently
    // degraded. `reaper` is None for embedded/test servers (field omitted).
    let reaper_json = reaper.map(|health| {
        use std::sync::atomic::Ordering::Relaxed;
        let armed = health.armed.load(Relaxed);
        let visibility_ok = health.visibility_ok.load(Relaxed);
        let kill_denied = health.kill_denied_total.load(Relaxed);

        if !armed {
            verdicts.push(serde_json::json!({
                "level": "PASS",
                "message": "reaper: not armed (side daemon)"
            }));
        } else if visibility_ok {
            verdicts.push(serde_json::json!({
                "level": "PASS",
                "message": "reaper: armed, system-wide process visibility"
            }));
        } else {
            verdicts.push(serde_json::json!({
                "level": "WARN",
                "message": "reaper visibility degraded: PID 1 not visible in process enumeration — this daemon runs confined (sandbox-spawned?) and cannot police daemons outside its confinement; restart it from an unconfined shell"
            }));
        }
        if armed && kill_denied > 0 {
            verdicts.push(serde_json::json!({
                "level": "WARN",
                "message": format!(
                    "reaper: {kill_denied} kill attempt(s) denied by the OS — orphans visible but unsignalable"
                )
            }));
        }

        serde_json::json!({
            "armed": armed,
            "visibility": if visibility_ok { "system-wide" } else { "confined" },
            "sweeps": health.sweeps_total.load(Relaxed),
            "reaped": health.reaped_total.load(Relaxed),
            "kill_denied": kill_denied,
        })
    });

    let data = serde_json::json!({
        "pid": std::process::id(),
        "version": env!("BEACHCOMBER_VERSION"),
        "uptime_secs": uptime_secs,
        "socket_path": socket_path.to_string_lossy().as_ref(),
        "config_path": config_path,
        "requests_total": requests_total.load(Ordering::Relaxed),
        "in_flight": in_flight_count,
        "active_watchers": active_watchers,
        "cache_entries": cache_entries,
        "watch_backend": watch_backend,
        "reaper": reaper_json,
        "verdicts": verdicts,
    });

    Response::ok(data, 0, false)
}

fn summarize_invalidation(strategy: &crate::provider::InvalidationStrategy) -> String {
    use crate::provider::InvalidationStrategy;
    match strategy {
        InvalidationStrategy::Poll { interval_secs } => format!("poll {interval_secs}s"),
        InvalidationStrategy::Watch { patterns, .. } => {
            if patterns.is_empty() {
                "watch (abs_paths)".to_string()
            } else {
                format!("watch {}", patterns.join(","))
            }
        }
        InvalidationStrategy::WatchAndPoll {
            patterns,
            interval_secs,
            ..
        } => {
            let pats = if patterns.is_empty() {
                "(abs_paths)".to_string()
            } else {
                patterns.join(",")
            };
            format!("watch {pats} + poll {interval_secs}s")
        }
    }
}

async fn handle_introspect_providers(
    registry: &ProviderRegistry,
    scheduler: Option<&SchedulerHandle>,
) -> Response {
    let backoff_list: Vec<LifecycleInfo> = if let Some(s) = scheduler {
        s.get_status()
            .await
            .map(|st| st.lifecycle)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut providers_out = Vec::new();
    let mut verdicts = Vec::new();

    let mut names = registry.list();
    names.sort();

    for name in &names {
        let (is_static, is_real) = (
            registry.is_static_namespace(name),
            registry.provider_metadata(name).is_some(),
        );

        let source_type = if is_static {
            "static"
        } else if is_real {
            "builtin"
        } else {
            "unknown"
        };

        let (scope, sources_out, invalidation) = if let Some(meta) =
            registry.provider_metadata(name)
        {
            // Infer scope from per-source metadata.
            let any_path = meta
                .sources
                .iter()
                .any(|s| s.scope == SourceScope::PathScoped);
            let scope = if any_path { "path" } else { "global" };

            // Collect per-source info.
            let sources_out: Vec<serde_json::Value> = meta
                .sources
                .iter()
                .map(|sm| {
                    let fields: Vec<serde_json::Value> = sm
                        .fields
                        .iter()
                        .map(|f| {
                            let type_str = match f.field_type {
                                crate::provider::FieldType::String => "string",
                                crate::provider::FieldType::Int => "int",
                                crate::provider::FieldType::Bool => "bool",
                                crate::provider::FieldType::Float => "float",
                                crate::provider::FieldType::Object => "object",
                            };
                            serde_json::json!({
                                "name": f.name,
                                "type": type_str,
                            })
                        })
                        .collect();
                    serde_json::json!({
                        "name": sm.name,
                        "scope": if sm.scope == SourceScope::PathScoped { "path" } else { "global" },
                        "fields": fields,
                        "invalidation": summarize_invalidation(&sm.invalidation),
                    })
                })
                .collect();

            // Use the first source's invalidation for the top-level summary.
            let invalidation = meta
                .sources
                .first()
                .map(|sm| summarize_invalidation(&sm.invalidation))
                .unwrap_or_else(|| "data-only".to_string());

            (scope, sources_out, invalidation)
        } else {
            ("global", Vec::new(), "data-only".to_string())
        };

        let relevant: Vec<&LifecycleInfo> = backoff_list
            .iter()
            .filter(|b| &b.provider == name)
            .collect();
        let in_backoff = if relevant.is_empty() {
            serde_json::Value::Null
        } else {
            let worst = relevant.iter().max_by_key(|b| b.elapsed_secs).unwrap();
            serde_json::json!({
                "stage": worst.stage,
                "elapsed_secs": worst.elapsed_secs,
            })
        };

        if !relevant.is_empty() {
            let worst = relevant.iter().max_by_key(|b| b.elapsed_secs).unwrap();
            verdicts.push(serde_json::json!({
                "level": "WARN",
                "message": format!("{name} in backoff {}s (stage={})", worst.elapsed_secs, worst.stage)
            }));
        }

        providers_out.push(serde_json::json!({
            "name": name,
            "source": source_type,
            "scope": scope,
            "sources": sources_out,
            "invalidation": invalidation,
            "in_backoff": in_backoff,
        }));
    }

    verdicts.insert(
        0,
        serde_json::json!({
            "level": "PASS",
            "message": format!("{} providers registered", names.len()),
        }),
    );

    Response::ok(
        serde_json::json!({
            "providers": providers_out,
            "verdicts": verdicts,
        }),
        0,
        false,
    )
}

fn handle_introspect_config(config: &Config) -> Response {
    let path = Config::config_path_if_exists();
    let path_json = match path {
        Some(ref p) => serde_json::Value::String(p.display().to_string()),
        None => serde_json::Value::Null,
    };
    let provider_count = config.providers.len() as u64;
    let verdicts = vec![
        serde_json::json!({"level": "PASS", "message": "config parsed"}),
        serde_json::json!({"level": "PASS", "message": format!("{provider_count} provider definitions loaded")}),
    ];
    Response::ok(
        serde_json::json!({
            "path": path_json,
            "parsed": true,
            "errors": [],
            "provider_count_from_config": provider_count,
            "verdicts": verdicts,
        }),
        0,
        false,
    )
}

fn handle_introspect_cache(cache: &Cache) -> Response {
    let entries = cache.list_entries();
    let total = entries.len() as u64;
    let stale = entries.iter().filter(|e| e.stale).count() as u64;
    let ratio = if total == 0 {
        0.0_f64
    } else {
        stale as f64 / total as f64
    };

    let mut verdicts = vec![serde_json::json!({
        "level": "PASS",
        "message": format!("{total} entries"),
    })];
    if stale > 0 {
        verdicts.push(serde_json::json!({
            "level": "WARN",
            "message": format!("{stale} stale — run `comb status --filter stale=true` to inspect"),
        }));
    } else if total > 0 {
        verdicts.push(serde_json::json!({"level": "PASS", "message": "no stale entries"}));
    }

    Response::ok(
        serde_json::json!({
            "total_entries": total,
            "stale_entries": stale,
            "stale_ratio": ratio,
            "verdicts": verdicts,
        }),
        0,
        false,
    )
}

async fn handle_introspect_lifecycle(scheduler: Option<&SchedulerHandle>) -> Response {
    let lifecycle: Vec<LifecycleInfo> = if let Some(s) = scheduler {
        s.get_status()
            .await
            .map(|st| st.lifecycle)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut verdicts = Vec::new();
    if lifecycle.is_empty() {
        verdicts.push(serde_json::json!({"level": "PASS", "message": "no providers in decay"}));
    } else {
        for entry in &lifecycle {
            let label = match &entry.path {
                Some(p) => format!("{} ({p})", entry.provider),
                None => entry.provider.clone(),
            };
            verdicts.push(serde_json::json!({
                "level": "WARN",
                "message": format!("{label} — stage={} elapsed={}s", entry.stage, entry.elapsed_secs),
            }));
        }
    }

    let lifecycle_json: Vec<serde_json::Value> = lifecycle
        .iter()
        .map(|b| serde_json::to_value(b).unwrap_or(serde_json::Value::Null))
        .collect();

    Response::ok(
        serde_json::json!({
            "lifecycle": lifecycle_json,
            "verdicts": verdicts,
        }),
        0,
        false,
    )
}

async fn handle_introspect_watches(scheduler: Option<&SchedulerHandle>) -> Response {
    let paths: Vec<String> = if let Some(s) = scheduler {
        s.get_status()
            .await
            .map(|st| st.watched_paths)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let verdict = if paths.is_empty() {
        serde_json::json!({"level": "WARN", "message": "not watching any paths"})
    } else {
        serde_json::json!({"level": "PASS", "message": format!("watching {} paths", paths.len())})
    };

    Response::ok(
        serde_json::json!({
            "paths": paths,
            "verdicts": [verdict],
        }),
        0,
        false,
    )
}

async fn handle_introspect_timers(scheduler: Option<&SchedulerHandle>) -> Response {
    let timers: Vec<PollTimerInfo> = if let Some(s) = scheduler {
        s.get_status()
            .await
            .map(|st| st.poll_timers)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut verdicts = vec![serde_json::json!({
        "level": "PASS",
        "message": format!("{} poll timers", timers.len()),
    })];

    for t in &timers {
        if t.interval_secs > 0 && t.last_run_secs_ago > t.interval_secs * 2 {
            let label = match &t.path {
                Some(p) => format!("{} ({p})", t.provider),
                None => t.provider.clone(),
            };
            verdicts.push(serde_json::json!({
                "level": "WARN",
                "message": format!("{label} overdue (interval={}s, last={}s ago)", t.interval_secs, t.last_run_secs_ago),
            }));
        }
    }

    let timers_json: Vec<serde_json::Value> = timers
        .iter()
        .map(|t| serde_json::to_value(t).unwrap_or(serde_json::Value::Null))
        .collect();

    Response::ok(
        serde_json::json!({
            "timers": timers_json,
            "verdicts": verdicts,
        }),
        0,
        false,
    )
}

async fn handle_introspect_demand(scheduler: Option<&SchedulerHandle>) -> Response {
    let demand: Vec<DemandInfo> = if let Some(s) = scheduler {
        s.get_status().await.map(|st| st.demand).unwrap_or_default()
    } else {
        Vec::new()
    };

    let verdict = serde_json::json!({
        "level": "PASS",
        "message": format!("{} active keys", demand.len()),
    });

    let demand_json: Vec<serde_json::Value> = demand
        .iter()
        .map(|d| serde_json::to_value(d).unwrap_or(serde_json::Value::Null))
        .collect();

    Response::ok(
        serde_json::json!({
            "demand": demand_json,
            "verdicts": [verdict],
        }),
        0,
        false,
    )
}

fn handle_introspect_procs(duration_secs: Option<u64>) -> Response {
    let dur = duration_secs.unwrap_or(2);
    match crate::proc_snapshot::capture(dur) {
        Ok(result) => {
            let samples: Vec<serde_json::Value> = result
                .samples
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "command": s.command,
                        "count": s.count,
                        "category": s.category,
                    })
                })
                .collect();
            let suggestions: Vec<serde_json::Value> = result
                .replacement_suggestions
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "command_pattern": r.command_pattern,
                        "provider": r.provider,
                        "field": r.field,
                    })
                })
                .collect();
            let mut verdicts = vec![serde_json::json!({
                "level": "INFO",
                "message": format!("{}s sample: {} exec events", result.duration_secs, result.total),
            })];
            if !result.replacement_suggestions.is_empty() {
                verdicts.push(serde_json::json!({
                    "level": "WARN",
                    "message": format!(
                        "{} replacement opportunit{}: consider using `comb get` instead",
                        result.replacement_suggestions.len(),
                        if result.replacement_suggestions.len() == 1 { "y" } else { "ies" }
                    ),
                }));
            }
            Response::ok(
                serde_json::json!({
                    "duration_secs": result.duration_secs,
                    "samples": samples,
                    "replacement_suggestions": suggestions,
                    "verdicts": verdicts,
                }),
                0,
                false,
            )
        }
        Err(e) => Response::error(format!("procs snapshot failed: {e}")),
    }
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
        cache.put_source("git", Some("/repo"), "refs", refs_fields, None);

        let registry = ProviderRegistry::with_defaults();
        let target = KeyParse::Field("git".to_string(), "branch".to_string());
        let resp = read_watch_value(&cache, &registry, &target, Some("/repo"), Some("/repo"));
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
                "put",
                "",
                Value::Object([("x".to_string(), Value::Int(1))].into_iter().collect()),
                false,
                Some(1),
                &registry,
            )
            .expect("no source owns p.x");
        tokio::time::advance(std::time::Duration::from_secs(5)).await;
        cache
            .write_static(
                "p",
                None,
                "config:t.toml",
                "",
                Value::Object([("y".to_string(), Value::Int(2))].into_iter().collect()),
                false,
                Some(100),
                &registry,
            )
            .expect("no source owns p.y");

        let read = cache.read_merged("p", None, &registry).expect("both maps");

        assert_eq!(
            freshness(&read, "x"),
            Some((5000, true)),
            "the older record's own age, stale past its 1s ttl"
        );
        assert_eq!(
            freshness(&read, "y"),
            Some((0, false)),
            "the newer record's own age, fresh inside its 100s ttl"
        );
        assert_eq!(
            freshness(&read, ""),
            Some((5000, true)),
            "the assembled object takes the largest age and any record's staleness"
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
                "put",
                "tree",
                Value::Object(HashMap::new()),
                false,
                None,
                &registry,
            )
            .expect("no source owns p.tree");
        tokio::time::advance(std::time::Duration::from_secs(9)).await;
        // The near layer: a directory below it, filling that node in.
        cache
            .write_static(
                "p",
                Some("/x/y"),
                "put",
                "tree.left",
                Value::Int(1),
                false,
                None,
                &registry,
            )
            .expect("no source owns p.tree.left");

        let read = cache
            .read_merged("p", Some("/x/y"), &registry)
            .expect("both layers");

        assert_eq!(
            freshness(&read, "tree"),
            Some((0, false)),
            "the node reports the record that answers it, not the empty one it replaced"
        );
        assert_eq!(
            freshness(&read, ""),
            Some((0, false)),
            "and so does the root the node sits in"
        );
    }
}
