use crate::cache::{Cache, Layers, MergedRead};
use crate::config::Config;
use crate::protocol::{IntrospectSubject, Request, Response};
use crate::provider::registry::{ProviderRegistry, RegistryHandle};
use crate::provider::source_call::{in_failback_backoff, record_outcome};
use crate::query::{Ask, Found, Instance, InstanceWalk, KeyParse, QueryPlan};
use crate::scheduler::{
    DemandInfo, LifecycleInfo, PollTimerInfo, SchedulerHandle, SchedulerMessage,
};
use crate::watcher_registry::WatcherRegistry;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tracing::{debug, info, warn};

pub struct Server {
    socket_path: PathBuf,
    cache: Arc<Cache>,
    registry: RegistryHandle,
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
    /// A server on the default configuration, which reads no file: an
    /// embedded or test server. The daemon passes its own
    /// ([`Self::new_with_config`]).
    pub fn new(
        socket_path: PathBuf,
        cache: Arc<Cache>,
        registry: impl Into<RegistryHandle>,
        scheduler: Option<SchedulerHandle>,
        watchers: Arc<WatcherRegistry>,
    ) -> Self {
        Self {
            socket_path,
            cache,
            registry: registry.into(),
            scheduler,
            watchers,
            start_instant: Instant::now(),
            requests_total: Arc::new(AtomicU64::new(0)),
            active_watches: Arc::new(AtomicUsize::new(0)),
            config: Arc::new(Config::default()),
            reaper: None,
        }
    }

    pub fn new_with_config(
        socket_path: PathBuf,
        cache: Arc<Cache>,
        registry: impl Into<RegistryHandle>,
        scheduler: Option<SchedulerHandle>,
        watchers: Arc<WatcherRegistry>,
        config: Config,
    ) -> Self {
        Self {
            socket_path,
            cache,
            registry: registry.into(),
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
                    let registry = self.registry.clone();
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
    registry: RegistryHandle,
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
                    &mut reader,
                    &mut writer,
                )
                .await;
                return Ok(());
            }
            Ok(request) => {
                requests_total.fetch_add(1, Ordering::Relaxed);
                // One registry for the whole request: a reload part-way
                // through cannot mix two.
                let current = registry.current();
                let response = handle_request(
                    &request,
                    &cache,
                    &current,
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

/// The root of `provider`'s Source `source` for `dir`
/// ([`ProviderRegistry::instance_root`]), without holding an async worker for
/// a first render: a config Source's `root` line can run a command (`exec`, up
/// to its timeout), fetch a URL or call a plugin. The registry keeps each
/// rendered root per directory, so only the first request for a directory pays
/// for the render, on the blocking pool; a kept root, and a built-in's
/// filesystem walk, resolve in place.
async fn root_for(
    registry: &Arc<ProviderRegistry>,
    provider: &str,
    source: &str,
    dir: &str,
) -> Result<Found, String> {
    if registry.renders_root(provider, source)
        && registry.kept_root(provider, source, dir).is_none()
    {
        let (registry, provider, source, dir) = (
            Arc::clone(registry),
            provider.to_string(),
            source.to_string(),
            dir.to_string(),
        );
        return tokio::task::spawn_blocking(move || {
            registry.instance_root(&provider, &source, &dir)
        })
        .await
        .map_err(|e| format!("resolving the root failed: {e}"))?;
    }
    registry.instance_root(provider, source, dir)
}

/// The roots found so far by one request, so Sources that find theirs the same
/// way ([`crate::provider::Source::root_key`]) ask once per directory.
#[derive(Default)]
struct RootMemo {
    known: HashMap<(String, String, String), Found>,
    /// Whether any root was rendered for the first time.
    rendered: bool,
}

/// The next instance of `walk`'s chain, or `None` when the chain is over.
async fn next_instance(
    walk: &mut InstanceWalk,
    registry: &Arc<ProviderRegistry>,
    provider: &str,
    source: &str,
    memo: &mut RootMemo,
) -> Result<Option<Option<String>>, String> {
    let answer = match walk.ask() {
        Ask::Done => return Ok(None),
        Ask::Home => Found::Home,
        Ask::Root(dir) => {
            let how = registry
                .source(provider, source)
                .map_or_else(|| source.to_string(), |s| s.root_key());
            let key = (provider.to_string(), how, dir.clone());
            match memo.known.get(&key) {
                Some(known) => known.clone(),
                None => {
                    memo.rendered |= registry.renders_root(provider, source)
                        && registry.kept_root(provider, source, &dir).is_none();
                    let found = root_for(registry, provider, source, &dir).await?;
                    memo.known.insert(key, found.clone());
                    found
                }
            }
        }
    };
    walk.step(answer)
}

/// The Sources a plan reads, for the instances that matter to its layers: the
/// `put` layer is what `put` wrote, so no Source is involved.
fn read_demand(plan: &QueryPlan) -> crate::query::SourceDemand {
    if plan.layers == Layers::Put {
        crate::query::SourceDemand::Sources(Vec::new())
    } else {
        plan.demand.clone()
    }
}

/// Every instance of every Source `plan` reads, each Source's chain in full:
/// what a read that cannot stop at the first hit (a whole namespace, a `watch`,
/// the `:cache` report) consults. An error is a `root` that could not be worked
/// out.
async fn all_instances(
    registry: &Arc<ProviderRegistry>,
    plan: &QueryPlan,
) -> Result<Vec<Instance>, String> {
    let mut out = Vec::new();
    let mut memo = RootMemo::default();
    for source in crate::query::demanded_source_names(registry, &plan.provider, &read_demand(plan))
    {
        let mut walk =
            registry.instance_walk(&plan.provider, &source, plan.requested_path.as_deref());
        while let Some(root) =
            next_instance(&mut walk, registry, &plan.provider, &source, &mut memo).await?
        {
            out.push(Instance {
                source: source.clone(),
                root,
            });
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
async fn handle_watch(
    key: String,
    path: Option<String>,
    context_path: &Option<String>,
    cache: &Cache,
    handle: &RegistryHandle,
    scheduler: Option<&SchedulerHandle>,
    watchers: &Arc<WatcherRegistry>,
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
) {
    let requested = path.as_deref().or(context_path.as_deref());
    // A reload puts another registry in force; the watch's plan was made under
    // the one before it, so it is made again. Subscribed before the first plan
    // is made, so a reload during it is not missed.
    let mut swapped = handle.subscribe();
    let registry = swapped.borrow_and_update().clone();
    let mut setup = match watch_setup(&key, requested, registry, cache, scheduler, watchers).await {
        Ok(setup) => setup,
        Err(msg) => {
            let _ = write_watch_line(writer, &Response::error(msg)).await;
            return;
        }
    };

    // Send initial value
    let initial = setup.read(cache);
    if write_watch_line(writer, &initial).await.is_err() {
        return;
    }

    // Both halves decide whether a frame is new. A template edited on its own
    // changes what the client renders just as much as a value does, so a frame
    // whose only change is the template is still a frame.
    let mut last = (initial.data.clone(), initial.template.clone());

    // Stream loop. A watch owns its connection for its whole lifetime — after
    // the initial request the client never writes again — so any completion
    // of the read races the write-triggered path: `Ok(0)` EOF, an error, or
    // even `Ok(n>0)` (a protocol-violating extra write) all mean this
    // connection is done being useful, and the watch ends. This is what lets
    // a dead client's watch drop at once instead of lingering until the next
    // write to the watched key. ("At once" has one hole: a `sched.send` into
    // a full scheduler queue parks this task until the queue drains.)
    //
    // The `notified()` future MUST be constructed inside the loop: a
    // `notify_one` landing mid-write stores a permit that the next fresh
    // `notified().await` consumes. Hoisting it out re-registers nothing and
    // becomes a genuine lost-wakeup bug.
    let mut probe = [0u8; 1];
    loop {
        tokio::select! {
            _ = setup.chain_watch.changed.notified() => {
                // Signal ongoing demand
                if !setup.layered && let Some(sched) = scheduler {
                    demand_watched(sched, &setup.provider_name, &setup.instances, &setup.called).await;
                }
            }
            changed = swapped.changed() => {
                if changed.is_err() {
                    break;
                }
                let registry = swapped.borrow_and_update().clone();
                setup = match watch_setup(&key, requested, registry, cache, scheduler, watchers).await {
                    Ok(setup) => setup,
                    Err(msg) => {
                        let _ = write_watch_line(writer, &Response::error(msg)).await;
                        break;
                    }
                };
            }
            r = reader.read(&mut probe) => {
                match r {
                    Ok(0) => debug!("watch ended: client closed the connection"),
                    Ok(n) => debug!("watch closed: client wrote {n} unexpected byte(s)"),
                    Err(e) => debug!("watch read ended: {e}"),
                }
                break;
            }
        }

        let response = setup.read(cache);

        // Field-level filtering: skip if neither half changed.
        let current = (response.data.clone(), response.template.clone());
        if current == last {
            continue;
        }
        last = current;

        if write_watch_line(writer, &response).await.is_err() {
            break; // Client disconnected
        }
    }
}

/// What a `watch` follows, made under one registry: the plan for its key, the
/// instances it reads and those its provider's lines call, and the
/// subscription to every entry the read walks. Made again when a reload puts
/// another registry in force.
struct WatchSetup {
    registry: Arc<ProviderRegistry>,
    plan: QueryPlan,
    provider_name: String,
    requested_path: Option<String>,
    /// A layer facet is a plain read: a watch on one warms nothing.
    layered: bool,
    instances: Vec<Instance>,
    called: Vec<(String, Vec<Instance>)>,
    chain_watch: ChainWatch,
}

impl WatchSetup {
    fn read(&self, cache: &Cache) -> Response {
        read_watch_value(
            cache,
            &self.registry,
            &self.plan.target,
            self.requested_path.as_deref(),
            &self.instances,
            self.plan.layers,
        )
    }
}

/// Make a watch's [`WatchSetup`] under `registry`, signal demand for what it
/// reads and refresh its read-always Sources. The error is for the client.
async fn watch_setup(
    key: &str,
    requested: Option<&str>,
    registry: Arc<ProviderRegistry>,
    cache: &Cache,
    scheduler: Option<&SchedulerHandle>,
    watchers: &Arc<WatcherRegistry>,
) -> Result<WatchSetup, String> {
    let plan = QueryPlan::build(key, requested, &registry)?;
    let provider_name = plan.provider.clone();

    // Facet syntax is checked on the provider segment (mirrors the Get path).
    crate::query::parse_facet(&provider_name)?;

    // Both halves resolve up the chain from the directory the caller named; the
    // provider's own records sit at the instances the read reaches — see
    // [`read_watch_value`].
    let requested_path = plan.requested_path.clone();

    // A layer facet is a plain read: a watch on one warms nothing.
    let layered = plan.layers != crate::cache::Layers::All;

    // A watch follows the whole chain of every Source it reads: it cannot stop
    // at the first hit, because a later change can make a nearer instance miss.
    let instances = all_instances(&registry, &plan).await.inspect_err(|msg| {
        warn!("watch {key}: {msg}");
    })?;

    // The Sources the provider's lines call: their chains are followed too.
    let called = if layered || registry.lines().all_calls(&provider_name).is_empty() {
        Vec::new()
    } else {
        called_instances(&registry, &provider_name, requested_path.as_deref())
            .await
            .map(|(called, _)| called)
            .inspect_err(|msg| warn!("watch {key}: {msg}"))?
    };

    // Signal demand for every instance on the chains (source-aware: a field
    // demands only its owning Source).
    if !layered && let Some(sched) = scheduler {
        demand_watched(sched, &provider_name, &instances, &called).await;
    }

    // Subscribe to every entry the read walk visits, not just the addressed
    // one: a `put` at an ancestor, at a directory below the provider's scope
    // entry, or at the global entry changes what this watch resolves to, so it
    // has to wake the stream. That is exactly what [`Cache::read_merged`]
    // walks (`query::read_chain`, which carries the roots of the instances even
    // when one sits above `$HOME`). Every write to `p` or to one of its facets
    // notifies `p` (the watcher registry's one key per namespace), so this one
    // chain also covers template edits and facet keys. One forwarder task per
    // link funnels the per-key broadcasts into a single channel.
    let chain_watch = {
        let changed = Arc::new(tokio::sync::Notify::new());
        let mut tasks = Vec::new();
        let mut links: Vec<(String, Option<String>)> = crate::query::read_chain(
            requested_path.as_deref(),
            &crate::query::roots_of(&instances),
        )
        .into_iter()
        .map(|layer| (provider_name.clone(), layer))
        .collect();
        // A change to a Source the lines call changes what they give.
        for (called_provider, instances) in &called {
            links.extend(
                crate::query::read_chain(
                    requested_path.as_deref(),
                    &crate::query::roots_of(instances),
                )
                .into_iter()
                .map(|layer| (called_provider.clone(), layer)),
            );
        }
        for (namespace, layer) in links {
            let mut sub = watchers.subscribe(namespace.as_str(), layer.as_deref());
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

    // Re-execute any read-always source(s) for the snapshot so the value is
    // fresh off disk, not stale cache.
    if !layered {
        for instance in &instances {
            maybe_read_always(
                &registry,
                cache,
                &provider_name,
                &instance.source,
                instance.root.as_deref(),
            )
            .await;
        }
    }

    Ok(WatchSetup {
        registry,
        plan,
        provider_name,
        requested_path,
        layered,
        instances,
        called,
        chain_watch,
    })
}

/// Signal a watch's demand: the instances it reads, and those of the Sources
/// its provider's lines call.
async fn demand_watched(
    scheduler: &SchedulerHandle,
    provider: &str,
    instances: &[Instance],
    called: &[(String, Vec<Instance>)],
) {
    scheduler
        .send(SchedulerMessage::QueryActivity {
            provider: provider.to_string(),
            instances: instances.to_vec(),
        })
        .await;
    for (called_provider, instances) in called {
        scheduler
            .send(SchedulerMessage::QueryActivity {
                provider: called_provider.clone(),
                instances: instances.clone(),
            })
            .await;
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

/// Cold-miss inline-execute: synchronously run one instance of a Source to
/// populate the cache, then let the caller re-read. Mirrors the scheduler's
/// execute_source path (spawn_blocking + cache.put_source) but runs in the
/// request task so the response carries fresh data (canon §"Cold cache miss
/// triggers inline fetch").
///
/// `root` is the instance: the cache address its records are written at, `None`
/// for the instance at `~`. Returns true if the Source ran and its result was
/// written, an empty result included; a failure is logged, writes nothing and
/// counts toward the instance's failback. An instance in failback backoff is not
/// run unless `forced`.
///
/// The cache *address* and the *directory* the Source runs in are two things:
/// the global entry's address is `None` and its directory is `$HOME`
/// (`query::normalise_path`), so the instance at `~` runs at `$HOME` and its
/// records land on the pathless entry.
async fn inline_execute_source(
    registry: &ProviderRegistry,
    cache: &Cache,
    provider: &str,
    source_name: &str,
    root: Option<&str>,
    forced: bool,
    skip_if_stored: bool,
) -> Result<bool, String> {
    let Some(source) = registry.source(provider, source_name) else {
        return Ok(false);
    };
    // An instance in failback backoff is left alone by a read, as the
    // scheduler leaves it alone; only an explicit `--force` or `refresh` tries
    // it again.
    if !forced && in_failback_backoff(registry, provider, source_name, root) {
        debug!(
            "Source '{provider}.{source_name}' at {root:?} is in failback backoff: not run by this read"
        );
        return Ok(false);
    }
    // One run of an instance at a time, whoever asks: a call from another
    // Source's lines takes the same turn. A read-always Source is cheap and
    // runs on every read, so it takes none.
    let turn = (!source.read_always()).then(|| registry.cold_lock(provider, source_name, root));
    let result = async {
        let mut held = None;
        let mut arrived = 0;
        if let Some(turn) = &turn {
            arrived = turn.finished.load(std::sync::atomic::Ordering::SeqCst);
            held = Some(turn.last.lock().await);
        }
        if skip_if_stored && held.is_some() {
            // The run this read waited for stored the instance, or failed it.
            if cache.has_provider_records_for(provider, root, source_name) {
                return Ok(true);
            }
            if in_failback_backoff(registry, provider, source_name, root) {
                return Ok(false);
            }
            if let (Some(turn), Some(Some(Err((message, fault))))) = (&turn, held.as_deref())
                && turn.finished.load(std::sync::atomic::Ordering::SeqCst) > arrived
            {
                return if *fault {
                    Err(message.clone())
                } else {
                    Ok(false)
                };
            }
        }
        let dir = crate::query::instance_dir(root);
        let src_clone = std::sync::Arc::clone(&source);
        // A run a read starts is cut off at the Source's timeout, as one the
        // scheduler starts is.
        let timeout = registry
            .source_metadata(provider, source_name)
            .map_or(crate::scheduler::lifecycle::DEFAULT_TIMEOUT, |m| m.timeout);
        let run = tokio::time::timeout(
            timeout,
            tokio::task::spawn_blocking(move || {
                crate::provider::source_call::ran(|| src_clone.execute(dir.as_deref()))
            }),
        )
        .await;
        let (outcome, failed_call) = match run {
            Ok(Ok(ran)) => ran,
            Ok(Err(_)) => return Ok(false),
            Err(_) => (Err(crate::scheduler::timed_out(timeout)), None),
        };
        let recorded = record_outcome(registry, cache, provider, source_name, root, outcome)
            .map_err(|e| (e, failed_call.is_some()));
        if let (Some(turn), Some(held)) = (&turn, held.as_mut()) {
            **held = Some(recorded.clone());
            turn.finished
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        match recorded {
            Ok(()) => Ok(true),
            Err((e, fault)) => {
                warn!("Source '{provider}.{source_name}' failed: {e}");
                // A run that failed because a Source it calls failed, or
                // called itself, is a fault in the lines, not in the world:
                // the read shows it, as it shows a `root` that fails. Any
                // other failure is a miss, and the read goes on to the rest
                // of the chain.
                if fault { Err(e) } else { Ok(false) }
            }
        }
    }
    .await;
    if let Some(turn) = turn {
        registry.release_cold(provider, source_name, root, turn);
    }
    result
}

/// How far a read goes to bring a Source up to date before it is read.
#[derive(Clone, Copy)]
enum Warm {
    /// Execute only when the instance has nothing cached (or the Source is
    /// read-always).
    Cold,
    /// Also re-execute when the instance's own map is past its interval
    /// (`--wait`).
    Stale,
    /// Always drop the Source's map and re-execute (`--force`, `refresh`).
    Always,
}

/// Bring one instance of a Source up to date for a read, and say whether it
/// ran. `root` is the instance: the cache entry its records live at.
///
/// The decision is per instance, never per provider: a warm sibling says nothing
/// about this one, so a cold instance is executed even when the namespace
/// already holds records. It runs when the Source is `read_always`, when `warm`
/// asks for it, or when it has nothing cached at its root.
///
/// The three modes differ only in when the Source's map is dropped first.
/// [`Warm::Cold`] never drops it and runs only on a cold miss. [`Warm::Stale`]
/// (`--wait`) drops it when it is past its own interval. [`Warm::Always`]
/// (`--force`, `refresh`) drops it unconditionally.
///
/// Dropping that one map rather than the entry is what keeps a re-execution
/// from taking its neighbours with it: the static records sharing the entry
/// survive.
async fn warm_source(
    registry: &ProviderRegistry,
    cache: &Cache,
    provider: &str,
    source_name: &str,
    root: Option<&str>,
    warm: Warm,
) -> Result<bool, String> {
    let Some(source) = registry.source(provider, source_name) else {
        return Ok(false);
    };
    let forced = matches!(warm, Warm::Always);
    let drop_first = match warm {
        Warm::Always => true,
        Warm::Stale => cache.source_is_stale(provider, root, source_name),
        Warm::Cold => false,
    };
    // A warm instance is the common read: answered before anything global is
    // locked.
    if !drop_first
        && !source.read_always()
        && cache.has_provider_records_for(provider, root, source_name)
    {
        return Ok(false);
    }
    // An instance in failback backoff keeps what it has: nothing is dropped
    // for a run that will not happen.
    if !forced && in_failback_backoff(registry, provider, source_name, root) {
        debug!(
            "Source '{provider}.{source_name}' at {root:?} is in failback backoff: not run by this read"
        );
        return Ok(false);
    }
    if drop_first {
        cache.remove_source(provider, root, source_name);
    }
    inline_execute_source(
        registry,
        cache,
        provider,
        source_name,
        root,
        forced,
        !drop_first,
    )
    .await?;
    Ok(true)
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
    root: Option<&str>,
) {
    if let Some(src) = registry.source(provider, source_name)
        && src.read_always()
    {
        // The failure is logged; a watch has no read to fail.
        let _ =
            inline_execute_source(registry, cache, provider, source_name, root, false, false).await;
    }
}

/// The instances, by provider, of the Sources `provider`'s `result` and
/// `return` lines call, as a read from `dir` reaches them, and whether a `root`
/// was rendered for the first time to find them. A `root` that needs rendering
/// is rendered here, off the async workers.
async fn called_instances(
    registry: &Arc<ProviderRegistry>,
    provider: &str,
    dir: Option<&str>,
) -> Result<(Vec<(String, Vec<Instance>)>, bool), String> {
    let called: Vec<(String, String)> = registry.lines().all_calls(provider).to_vec();
    let mut out: Vec<(String, Vec<Instance>)> = Vec::new();
    let mut memo = RootMemo::default();
    for (called_provider, source) in &called {
        // A disabled provider has no instances: its Source is not run for a
        // call, and the call is a miss.
        if registry.is_disabled(called_provider) {
            continue;
        }
        let mut walk = registry.instance_walk(called_provider, source, dir);
        let mut found = Vec::new();
        while let Some(root) =
            next_instance(&mut walk, registry, called_provider, source, &mut memo).await?
        {
            found.push(Instance {
                source: source.clone(),
                root,
            });
        }
        match out.iter_mut().find(|(p, _)| p == called_provider) {
            Some((_, instances)) => instances.extend(found),
            None => out.push((called_provider.clone(), found)),
        }
    }
    Ok((out, memo.rendered))
}

/// Bring up to date, as `how` says, every Source of another provider that
/// `provider`'s `result` and `return` lines call, at each instance a read from
/// `dir` reaches, and say which those were and whether the read must be made
/// again: a Source ran, or a root was resolved that the first read did not
/// have. What the lines then render over is stored.
async fn warm_called(
    registry: &Arc<ProviderRegistry>,
    cache: &Cache,
    provider: &str,
    dir: Option<&str>,
    how: Warm,
) -> Result<(Vec<(String, Vec<Instance>)>, bool), String> {
    let (called, resolved) = called_instances(registry, provider, dir).await?;
    let mut ran = resolved;
    for (called_provider, instances) in &called {
        for instance in instances {
            ran |= warm_source(
                registry,
                cache,
                called_provider,
                &instance.source,
                instance.root.as_deref(),
                how,
            )
            .await?;
        }
    }
    Ok((called, ran))
}

/// What a climb does at each instance it reaches.
#[derive(Clone, Copy)]
enum Climb {
    /// Bring the instance up to date as `Warm` says, then read.
    Warm(Warm),
    /// Only find the instances; run nothing (`refresh` hands them to the
    /// scheduler).
    Select,
}

/// Where a climb got to.
struct Climbed {
    /// The instances brought up to date and demanded, nearest first: every
    /// one visited but those a `put` answered.
    reached: Vec<Instance>,
    /// Every instance visited, one a `put` answered included (which is not in
    /// `reached`: nothing runs or is kept warm for it).
    visited: Vec<Instance>,
    /// The merged read at the end.
    merged: Option<MergedRead>,
}

/// One demanded Source's walk up its instances, with the next root found but
/// not yet visited.
struct Walking {
    name: String,
    walk: InstanceWalk,
    next: Option<Option<String>>,
    over: bool,
}

/// How near a root is to the directory read: deeper is nearer, and the
/// instance at `~` is the farthest of all.
fn nearness(root: &Option<String>) -> i64 {
    match root {
        Some(root) => std::path::Path::new(root).components().count() as i64,
        None => -1,
    }
}

/// Whether a merged read answers a field read: only a leaf does. An object is
/// the union of what each instance says, so it is complete only at `~`, the
/// same as a namespace read.
fn answers_field(merged: &Option<MergedRead>, target: &KeyParse) -> bool {
    match target {
        KeyParse::Field(_, field) => lookup_merged(merged, field).is_some_and(|v| !v.is_object()),
        KeyParse::Provider(_) => false,
    }
}

/// The instance chains of every demanded Source, climbed together nearest
/// first, until the merged read answers or every chain is over.
///
/// Every Source is asked for its next root before any instance is visited, and
/// the nearest root among them is visited next, `~` last. A Source whose next
/// root is far cannot therefore answer before another Source's nearer one.
/// After each visit the merged read is tried: a field read stops on a leaf; a
/// namespace read, and a field that is an object, take every level.
///
/// `honor_put` lets a field a `put` states at an instance be answered there,
/// without running the Source it hides.
async fn climb(
    registry: &Arc<ProviderRegistry>,
    cache: &Cache,
    plan: &QueryPlan,
    demanded: &[String],
    mode: Climb,
    honor_put: bool,
) -> Result<Climbed, String> {
    let provider = plan.provider.as_str();
    // Under the shipped lines a field is its Source's, and a `put` or a hit
    // settles it early. Under any other a line decides what exists, so a read
    // takes every level and honours no put's shortcut.
    let by_default = registry.reads_by_default(provider);
    let honor_put = honor_put && by_default;
    let mut walking: Vec<Walking> = demanded
        .iter()
        .map(|name| Walking {
            name: name.clone(),
            walk: registry.instance_walk(provider, name, plan.requested_path.as_deref()),
            next: None,
            over: false,
        })
        .collect();
    let mut memo = RootMemo::default();
    let mut out = Climbed {
        reached: Vec::new(),
        visited: Vec::new(),
        merged: None,
    };
    loop {
        for w in walking.iter_mut() {
            if w.next.is_none() && !w.over {
                match next_instance(&mut w.walk, registry, provider, &w.name, &mut memo).await? {
                    Some(root) => w.next = Some(root),
                    None => w.over = true,
                }
            }
        }
        let nearest = walking
            .iter()
            .filter_map(|w| w.next.as_ref().map(nearness))
            .max();
        let Some(nearest) = nearest else {
            if out.merged.is_none() {
                out.merged = read_chain_merged(cache, registry, plan, &out.visited);
            }
            return Ok(out);
        };
        for w in walking.iter_mut() {
            if w.next.as_ref().is_none_or(|root| nearness(root) != nearest) {
                continue;
            }
            let root = w.next.take().expect("checked above");
            out.visited.push(Instance {
                source: w.name.clone(),
                root: root.clone(),
            });
            if honor_put
                && matches!(&plan.target, KeyParse::Field(_, field)
                    if cache.put_answers(provider, root.as_deref(), field))
            {
                continue;
            }
            if let Climb::Warm(how) = mode {
                warm_source(registry, cache, provider, &w.name, root.as_deref(), how).await?;
            }
            out.reached.push(Instance {
                source: w.name.clone(),
                root,
            });
        }
        out.merged = read_chain_merged(cache, registry, plan, &out.visited);
        if by_default && answers_field(&out.merged, &plan.target) {
            return Ok(out);
        }
    }
}

/// The merged read of `plan`'s address over the `visited` instances.
fn read_chain_merged(
    cache: &Cache,
    registry: &ProviderRegistry,
    plan: &QueryPlan,
    visited: &[Instance],
) -> Option<MergedRead> {
    cache.read_merged(
        &plan.provider,
        plan.requested_path.as_deref(),
        visited,
        registry,
        plan.layers,
    )
}

/// The value at `field` in a merged read, as wire JSON.
///
/// `provider::Value` is `#[serde(untagged)]` over JSON-native shapes, so the
/// conversion cannot fail; `.ok()` rather than a `Null` stand-in so that if it
/// somehow did, the address reads as absent instead of as a stored null. Every
/// conversion out of a merged tree takes that line.
fn lookup_merged(merged: &Option<MergedRead>, field: &str) -> Option<serde_json::Value> {
    let value = crate::provider::lookup_path(&merged.as_ref()?.fields, field)?;
    serde_json::to_value(value).ok()
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
    let mut oldest: Option<u128> = None;
    let mut stale = false;
    for (tail, record) in &read.records {
        if !libbeachcomber::key::within(tail, rest) {
            continue;
        }
        let age = record.age_ms();
        oldest = Some(oldest.map_or(age, |o| o.max(age)));
        stale |= record.is_stale();
    }
    if oldest.is_some() {
        return oldest.map(|age| (age, stale));
    }
    // Inside a leaf — an array's element: the record is the array's own.
    let mut at = rest.to_string();
    while let Some(up) = libbeachcomber::key::parent(&at) {
        if let Some(record) = read.records.get(&up) {
            return Some((record.age_ms(), record.is_stale()));
        }
        if up.is_empty() {
            break;
        }
        at = up;
    }
    None
}

/// What the template half of a `get` found. `Default` is "nothing attached",
/// which is every early return in [`template_half`].
#[derive(Default)]
struct TemplateHalf {
    /// The node at the requested address in the `template` facet's merged tree.
    template: Option<serde_json::Value>,
    /// The part of the address the walk could not consume.
    rest: Option<String>,
}

/// What a `get` answers once the provider's `return` has read the data: the
/// data it hands out, and the template half beside it.
struct Answered {
    merged: Option<MergedRead>,
    half: TemplateHalf,
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
fn answer(
    cache: &Cache,
    registry: &ProviderRegistry,
    provider: &str,
    rest: &str,
    path: Option<&str>,
    result: Option<MergedRead>,
) -> Answered {
    let returned = crate::shape::provider::returned_over(cache, registry, provider, path, result);
    Answered {
        half: template_half(returned.template, rest),
        merged: returned.read,
    }
}

/// The node at `rest` in the `template` facet a `return` attached.
fn template_half(
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
            rest: None,
        };
    }

    let Some((head, mut left)) = libbeachcomber::key::split_first(rest) else {
        return TemplateHalf::default();
    };
    let Some(mut current) = read.fields.get(&head) else {
        return TemplateHalf::default();
    };
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
                };
            }
            _ => return TemplateHalf::default(),
        }
        left = below;
    }
    match current {
        Value::Object(_) | Value::String(_) => TemplateHalf {
            template: serde_json::to_value(current).ok(),
            rest: None,
        },
        _ => TemplateHalf::default(),
    }
}

/// Assemble the response for a resolved read: the data half out of `merged`
/// with its own freshness, and the template half attached to it.
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
fn resolve_halves(
    merged: &Option<MergedRead>,
    target: &KeyParse,
    template: Option<serde_json::Value>,
    template_rest: Option<String>,
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
    response.with_template(template, template_rest)
}

// Watch reads only from cache — no inline-execute on cold miss (unlike Get).
// A cache miss in a streaming context means the scheduler has not populated the
// entry yet; the caller receives an update when it does.
fn read_watch_value(
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
    // A watch frame carries the same two halves a `get` does. A facet key is
    // already inside a facet and never gets one. The template decides whether
    // an unresolvable sub-path is a hit or a miss.
    let merged = cache.read_merged(provider, requested, reached, registry, layers);
    let Answered { merged, half } = if layers != crate::cache::Layers::All
        || crate::query::parse_facet(provider).ok().flatten().is_some()
    {
        Answered {
            merged,
            half: TemplateHalf::default(),
        }
    } else {
        // Templates are static records, so the template half resolves up the
        // chain from the directory the caller named, not from an instance's root.
        answer(cache, registry, provider, target.rest(), requested, merged)
    };
    resolve_halves(&merged, target, half.template, half.rest)
}

/// The answer of `get p:cache.<path>`: the tree of what is known about the
/// records a read of `p.<path>` resolves to, shaped like that read's value, with
/// a leaf answer at each leaf. The address resolves exactly as the read does
/// (same chain, nearest record), and nothing is warmed or run.
///
/// A leaf answers `age_ms`, `stale` and `source` (derived from the record that
/// answered), plus the keys stored beside it (`poll`, `watch`, `decay`). A leaf a
/// template answers is `{"stale": false, "source": "template"}`, with no
/// `age_ms`: the client renders it, so it is as old as the inputs the render
/// reads, which the daemon never sees. `p:cache._` is the prototype node, which
/// is not part of the value: it reads back as written.
fn cache_facet_response(
    cache: &Cache,
    registry: &ProviderRegistry,
    target: &KeyParse,
    requested: Option<&str>,
    reached: &[Instance],
) -> Response {
    let (KeyParse::Field(provider, _) | KeyParse::Provider(provider)) = target;
    let rest = target.rest();

    if rest == "_" || rest.starts_with("_.") {
        let prototypes = cache.read_merged(
            &format!("{provider}:cache"),
            requested,
            &[],
            registry,
            Layers::All,
        );
        return match prototypes.and_then(|m| {
            crate::provider::lookup_path(&m.fields, rest).and_then(|v| serde_json::to_value(v).ok())
        }) {
            Some(value) => Response::ok(value, 0, false),
            None => Response::miss(),
        };
    }

    let merged = cache.read_merged(provider, requested, reached, registry, Layers::All);
    let Answered { merged, half } = answer(cache, registry, provider, rest, requested, merged);
    let value = merged.as_ref().and_then(|m| {
        if rest.is_empty() {
            Some(crate::provider::Value::Object(m.fields.clone()))
        } else {
            crate::provider::lookup_path(&m.fields, rest).cloned()
        }
    });
    let leaf = |tail: &str| -> Option<serde_json::Value> {
        let record = merged.as_ref()?.records.get(tail)?;
        let mut answer = serde_json::Map::new();
        answer.insert("age_ms".into(), (record.age_ms() as u64).into());
        answer.insert("stale".into(), record.is_stale().into());
        let label: &str = &record.source;
        answer.insert("source".into(), crate::cache::presented_label(label).into());
        let stored = cache.stored_keys(provider, record.entry.as_deref(), label, tail);
        for (key, v) in stored.iter().flatten() {
            if let Ok(v) = serde_json::to_value(v) {
                answer.insert(key.clone(), v);
            }
        }
        // A Source's field without a decay of its own takes the prototype's.
        if !crate::cache::is_static_label(label) && !answer.contains_key("decay") {
            answer.insert(
                "decay".into(),
                cache
                    .resolve_decay(provider, record.entry.as_deref())
                    .into(),
            );
        }
        Some(answer.into())
    };

    match cache_facet_node(rest, value.as_ref(), half.template.as_ref(), &leaf) {
        Some(tree) => Response::ok(tree, 0, false),
        None => Response::miss(),
    }
}

/// The tree of leaf answers for the node at `tail`: the one place the answer
/// for an object node lives. `value` is the data at the node, `template` the
/// template tree at it; a template wins over data at the same leaf, as it does
/// in a read. An object with no answer anywhere below it is no answer.
fn cache_facet_node(
    tail: &str,
    value: Option<&crate::provider::Value>,
    template: Option<&serde_json::Value>,
    leaf: &dyn Fn(&str) -> Option<serde_json::Value>,
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
                    )?;
                    Some((k.clone(), answer))
                })
                .collect();
            (!tree.is_empty()).then(|| tree.into())
        }
        (_, Some(_)) => Some(serde_json::json!({
            "stale": false, "source": "template"
        })),
        (Some(Value::Object(map)), None) if !map.is_empty() => {
            let tree: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .filter_map(|(k, v)| {
                    Some((k.clone(), cache_facet_node(&child(k), Some(v), None, leaf)?))
                })
                .collect();
            (!tree.is_empty()).then(|| tree.into())
        }
        (Some(_), None) => leaf(tail),
        (None, None) => None,
    }
}

/// The `layers` answer for `plan`: per leaf of the node the key names, every
/// layer a read from the requested directory consults, in order — template
/// records, then a leaf a provider line computed, then each link's `put`,
/// `config(…)` seeds and Source records ([`Cache::layer_stacks`]) — with the
/// one the read takes marked `answered`. A leaf's list also holds a record
/// standing as a scalar on an ancestor of it, which hides the leaf in that
/// layer. A tree shaped like the value, an array at each leaf; a leaf key is
/// its array alone.
fn layers_response(
    cache: &Cache,
    registry: &ProviderRegistry,
    plan: &QueryPlan,
    instances: &[Instance],
) -> Response {
    use crate::cache::LayerRecord;
    let provider = plan.provider.as_str();
    let requested = plan.requested_path.as_deref();
    let merged = cache.read_merged(provider, requested, instances, registry, Layers::All);
    let Answered { merged, half } = answer(cache, registry, provider, "", requested, merged);

    let stacked = cache.layer_stacks(provider, requested, instances);
    let (mut ordered, data): (Vec<_>, Vec<_>) =
        stacked.into_iter().partition(|(_, r)| r.facet.is_some());
    // A leaf a provider line computed belongs to no record: it sits under
    // the templates and over the data it was computed from.
    if let Some(m) = &merged {
        let mut computed: Vec<(&String, &crate::cache::RecordFreshness)> = m
            .records
            .iter()
            .filter(|(_, r)| r.source.starts_with(crate::cache::COMPUTED_PREFIX))
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
                    record.is_stale(),
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

    let template_at = |tail: &str| -> bool {
        let mut node = half.template.as_ref();
        for segment in libbeachcomber::key::segments(tail) {
            match node {
                Some(serde_json::Value::String(_)) => return true,
                Some(serde_json::Value::Object(map)) => node = map.get(segment.as_ref()),
                _ => return false,
            }
        }
        matches!(node, Some(serde_json::Value::String(_)))
    };
    let answering = |tail: &str| -> Option<(String, Option<String>)> {
        let records = &merged.as_ref()?.records;
        let mut at = tail.to_string();
        loop {
            if let Some(r) = records.get(&at) {
                return Some((
                    crate::cache::presented_label(&r.source),
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
        if template_at(leaf) {
            if let Some(first) = list.iter_mut().find(|r| r.facet.is_some()) {
                first.answered = true;
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
        Some(list) => Response::ok(list, 0, false),
        None if tree.is_empty() => Response::miss(),
        None => Response::ok(serde_json::Value::Object(tree), 0, false),
    }
}

/// Set `value` at the address `segments` of `tree`, making the objects on
/// the way. Each segment is a key as stored, never split again.
fn insert_at<S: AsRef<str>>(
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
    registry: &Arc<ProviderRegistry>,
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
            // path, parsed target, and the warming demand. Get and Watch both
            // build this so they share identical key semantics.
            let requested = path.as_deref().or(context_path.as_deref());
            let plan = match QueryPlan::build(key, requested, registry) {
                Ok(plan) => plan,
                Err(msg) => return Response::error(msg),
            };
            let provider_name = plan.provider.as_str();

            // Facet syntax is checked on the provider segment produced by the plan.
            let facet = match crate::query::parse_facet(provider_name) {
                Ok(facet) => facet,
                Err(msg) => return Response::error(msg),
            };

            // `p:cache` reports on what is cached: nothing is warmed, no demand
            // is signalled, and no template is attached. It reports the records
            // a read would find, so it follows every chain in full.
            if plan.layers == crate::cache::Layers::Cache {
                let instances = match all_instances(registry, &plan).await {
                    Ok(instances) => instances,
                    Err(msg) => {
                        warn!("get {key}: {msg}");
                        return Response::error(msg);
                    }
                };
                return cache_facet_response(
                    cache,
                    registry,
                    &plan.target,
                    plan.requested_path.as_deref(),
                    &instances,
                );
            }

            // The two halves are addressed from different places. A template is
            // a static record, and static records resolve up the chain from the
            // directory the caller is standing in — so the template half reads
            // from the requested path, which the plan already canonicalised. The
            // data half reads the instances the walk below reaches.
            let template_path = plan.requested_path.as_deref();

            // `--force` re-executes below, per instance; nothing is evicted
            // here. Evicting the addressed entry wholesale used to take the
            // static records with it.
            if *force && registry.is_disabled(provider_name) {
                return Response::error(format!(
                    "cannot --force disabled provider '{provider_name}': no source to re-execute \
                     (enabled = false)"
                ));
            }
            if *force && registry.is_static_namespace(provider_name) {
                return Response::error(format!(
                    "cannot --force static namespace '{provider_name}': no source to re-execute from"
                ));
            }

            // Every Source the key demands is brought up to date on its own
            // terms — one cold source is no longer covered for by a warm
            // sibling — and each at its instances, nearest first. `plan.demand`
            // is every Source for a bare namespace and the owning Source for a
            // field, which is what makes this one loop serve both — and what
            // makes it the same set `refresh` re-executes. The `put` layer is
            // what `put` wrote: no Source has anything to run for it.
            let demanded =
                crate::query::demanded_source_names(registry, provider_name, &read_demand(&plan));
            let how = match (*force, *wait) {
                (true, _) => Warm::Always,
                (false, true) => Warm::Stale,
                (false, false) => Warm::Cold,
            };

            // The instance chains, climbed together nearest first ([`climb`]):
            // each instance is brought up to date and the merged read asked for
            // the address. A field read stops on a leaf; a namespace read has no
            // single address to satisfy, so it takes every level.
            let Climbed {
                reached,
                visited,
                mut merged,
            } = match climb(
                registry,
                cache,
                &plan,
                &demanded,
                Climb::Warm(how),
                !*force && plan.layers != crate::cache::Layers::Source,
            )
            .await
            {
                Ok(climbed) => climbed,
                Err(msg) => {
                    warn!("get {key}: {msg}");
                    return Response::error(msg);
                }
            };

            // The Sources the provider's lines call are brought up to date
            // before the lines render, which do no IO: a cold one runs here,
            // once. After the provider's own, so a callee that reads the
            // provider (`comb.<p>`) finds it warm. A provider whose lines call
            // nothing pays nothing.
            let called = if plan.layers != crate::cache::Layers::Put
                && registry.lines().has_calls(provider_name)
            {
                match warm_called(
                    registry,
                    cache,
                    provider_name,
                    plan.requested_path.as_deref(),
                    how,
                )
                .await
                {
                    Ok((called, changed)) => {
                        // The first read of the lines found no root resolved
                        // for the called Sources, or them cold: read again.
                        if changed {
                            merged = cache.read_merged(
                                provider_name,
                                plan.requested_path.as_deref(),
                                &visited,
                                registry,
                                plan.layers,
                            );
                        }
                        called
                    }
                    Err(msg) => {
                        warn!("get {key}: {msg}");
                        return Response::error(msg);
                    }
                }
            } else {
                Vec::new()
            };

            // A field the Source's last run left absent is absent until it runs
            // again, as a field it produced is present until then: the read is
            // answered from that run, never by running the Source once more.

            // Demand every instance the read reached, so each is kept warm and,
            // the first time, its schedule is registered. The Sources the
            // lines call are kept warm the same way, while they are called.
            if let Some(sched) = scheduler {
                for (called_provider, instances) in called {
                    sched
                        .send(SchedulerMessage::QueryActivity {
                            provider: called_provider,
                            instances,
                        })
                        .await;
                }
            }
            if let Some(sched) = scheduler
                && !reached.is_empty()
            {
                sched
                    .send(SchedulerMessage::QueryActivity {
                        provider: provider_name.to_string(),
                        instances: reached,
                    })
                    .await;
            }

            // The template half is what the provider's `return` attaches over
            // the data read: `get box.widgets.foo` where `box.widgets` is an
            // object without a `foo` is a hit when a template answers there, a
            // miss otherwise. Both halves, assembled the one way `watch`
            // assembles them.
            let Answered { merged, half } =
                if facet.is_some() || plan.layers != crate::cache::Layers::All {
                    Answered {
                        merged,
                        half: TemplateHalf::default(),
                    }
                } else {
                    answer(
                        cache,
                        registry,
                        provider_name,
                        plan.target.rest(),
                        template_path,
                        merged,
                    )
                };
            resolve_halves(&merged, &plan.target, half.template, half.rest)
        }
        Request::Refresh { key, path } => {
            // The same plan every other op builds, so the key is checked and
            // the path resolved exactly as for a `get`.
            let requested = path.as_deref().or(context_path.as_deref());
            let plan = match QueryPlan::build(key, requested, registry) {
                Ok(plan) => plan,
                Err(msg) => return Response::error(msg),
            };
            let provider_name = plan.provider.as_str();
            if let Err(msg) = crate::query::parse_facet(provider_name) {
                return Response::error(msg);
            }
            if plan.layers != crate::cache::Layers::All {
                return Response::error(format!(
                    "cannot refresh {key}: a layer facet is read-only"
                ));
            }

            // The same set `get` warms, re-executed instead of read. A key that
            // names none — a static namespace, a facet, an unknown name, or a
            // field no Source owns — has nothing to re-execute, and saying ok
            // would claim work that never happened. Reading configuration
            // afresh is not a refresh: the config watcher does that.
            let demanded =
                crate::query::demanded_source_names(registry, provider_name, &plan.demand);
            let addressed = match &plan.target {
                KeyParse::Provider(p) => p.clone(),
                KeyParse::Field(p, f) => libbeachcomber::key::join(p, f),
            };
            if registry.is_disabled(provider_name) {
                return Response::error(format!("nothing to refresh: {provider_name} is disabled"));
            }
            if demanded.is_empty() {
                return Response::error(format!("nothing to refresh: {addressed} has no sources"));
            }

            // The instances a `get --force` of the same key would re-run: every
            // instance for a namespace, and for a field those up to the one that
            // answers it. Found the way `get` finds them; whether the scheduler
            // runs them or this connection does.
            let instances =
                match climb(registry, cache, &plan, &demanded, Climb::Select, false).await {
                    Ok(climbed) => climbed.reached,
                    Err(msg) => {
                        warn!("refresh {key}: {msg}");
                        return Response::error(msg);
                    }
                };
            // Every demanded Source is nearest and found no instance here.
            if instances.is_empty() {
                return Response::error(format!(
                    "nothing to refresh: {addressed} has no instance at {}",
                    plan.requested_path.as_deref().unwrap_or("~")
                ));
            }
            match scheduler {
                Some(sched) => {
                    sched
                        .send(SchedulerMessage::Refresh {
                            provider: provider_name.to_string(),
                            instances,
                        })
                        .await;
                }
                None => {
                    for instance in &instances {
                        let warmed = warm_source(
                            registry,
                            cache,
                            provider_name,
                            &instance.source,
                            instance.root.as_deref(),
                            Warm::Always,
                        )
                        .await;
                        if let Err(msg) = warmed {
                            warn!("refresh {key}: {msg}");
                            return Response::error(msg);
                        }
                    }
                }
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
        } => {
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

            if let Some(base) = namespace
                .strip_suffix(":put")
                .or_else(|| namespace.strip_suffix(":source"))
            {
                return Response::error(format!(
                    "{base}:put and {base}:source are read-only; write {base}.<node>"
                ));
            }

            // Facet syntax is checked on the namespace segment.
            if let Err(msg) = crate::query::parse_facet(namespace) {
                return Response::error(msg);
            }

            // The one `:cache` key a put may write is a decay prototype. The
            // rest is derived at a read or stored by the daemon.
            let is_cache_facet = namespace.ends_with(":cache");
            if is_cache_facet {
                let derived = ["age_ms", "stale", "source"]
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

            // A template is linted as a configured one is: one that does not
            // compile is refused. A name no provider has is not, since a later
            // put may make that provider.
            if namespace.ends_with(":template") {
                let engine = libbeachcomber::engine::build_engine_env(Default::default());
                let at = libbeachcomber::key::join(namespace, node);
                let refused = crate::daemon::seed::lint_template_tree(&engine, data, &at, &[])
                    .into_iter()
                    .find(|(_, f)| matches!(f, libbeachcomber::eval::Finding::Compile(_)));
                if let Some((leaf, finding)) = refused {
                    return Response::error(format!("{leaf}: {finding}"));
                }
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
            let effective_path: Option<String> = match crate::query::normalise_path(path.as_deref())
            {
                Ok(p) => p,
                Err(e) => return Response::error(e),
            };

            // A ttl is the put's lifecycle: valid for N, stale for one decay
            // step, then gone. It needs a value to expire.
            if ttl.is_some() && data.is_null() {
                return Response::error("a ttl on an absent value has no meaning");
            }
            let ttl_secs = match ttl.as_deref() {
                None => None,
                Some(t) => match crate::config::parse_whole_seconds(t).map(|d| d.as_secs()) {
                    None => return Response::error(format!("invalid ttl: {t}")),
                    Some(0) => return Response::error("ttl must be at least 1s"),
                    Some(n) => Some(n),
                },
            };
            let put_written = || crate::scheduler::SchedulerMessage::PutWritten {
                provider: namespace.to_string(),
                path: effective_path.clone(),
                node: node.to_string(),
                ttl_secs,
            };

            // `data: null` means "clear the addressed node" — the row goes, the
            // registry entry stays.
            if data.is_null() {
                cache.remove_put(namespace, effective_path.as_deref(), node);
                if let Some(sched) = scheduler {
                    sched.send(put_written()).await;
                }
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

            if let Err(e) = cache.write_static(
                namespace,
                effective_path.as_deref(),
                crate::cache::PUT_SOURCE,
                node,
                value,
                ttl_secs,
            ) {
                return Response::error(e.to_string());
            }
            if let Some(sched) = scheduler {
                sched.send(put_written()).await;
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
                // A put row is looked up only when its address carries a ttl
                // (`Cache::list_rows` leaves its kind unset); without one it
                // stays static, whatever put sits above it.
                let is_put = row.source == crate::cache::PUT_SOURCE;

                if row.kind != Some(RowKind::Static) {
                    // Look up the lifecycle snapshot for THIS row's owning source
                    // so glyphs and TTL columns reflect the source's strategy
                    // (refs vs diff vs status all differ within a single provider).
                    // A put row's key is its own address: the row's field, or the
                    // root address when the field is a root put's top-level key.
                    let key = crate::scheduler::lifecycle::Key {
                        provider: row.provider.clone(),
                        path: row.path.clone(),
                        source: row.source.clone(),
                        node: if is_put {
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
                        row.fsevents_reinstate = Some(snap.fsevents_reinstate);
                    } else if is_put {
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

            // The internal labels keyed the lookups above; a reader gets the
            // presented form, as a `:cache` answer does.
            for row in rows.iter_mut() {
                row.source = crate::cache::presented_label(&row.source);
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
                    config.loaded_path(),
                )
            }
            IntrospectSubject::Providers => handle_introspect_providers(registry, scheduler).await,
            IntrospectSubject::Config => handle_introspect_config(config, registry),
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
        Request::Layers { key, path } => {
            let requested = path.as_deref().or(context_path.as_deref());
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
    loaded_config: Option<&std::path::Path>,
) -> Response {
    let config_path = loaded_config
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
        InvalidationStrategy::Poll { interval_secs, .. } => format!("poll {interval_secs}s"),
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

        let sourceless = registry
            .provider_metadata(name)
            .is_some_and(|m| m.sources.is_empty());
        let source_type = if is_static || sourceless {
            "static"
        } else if is_real {
            "builtin"
        } else {
            "unknown"
        };

        let disabled = registry.is_disabled(name);
        let (sources_out, invalidation) = if let Some(meta) = registry.provider_metadata(name) {
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
                        "fields": fields,
                        "invalidation": if disabled {
                            "disabled".to_string()
                        } else {
                            summarize_invalidation(&sm.invalidation)
                        },
                    })
                })
                .collect();

            // Use the first source's invalidation for the top-level summary.
            let invalidation = if disabled {
                "disabled".to_string()
            } else {
                meta.sources
                    .first()
                    .map(|sm| summarize_invalidation(&sm.invalidation))
                    .unwrap_or_else(|| "data-only".to_string())
            };

            (sources_out, invalidation)
        } else {
            (Vec::new(), "data-only".to_string())
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
                "message": format!("{name} in backoff {} (stage={})", secs(worst.elapsed_secs), worst.stage)
            }));
        }

        providers_out.push(serde_json::json!({
            "name": name,
            "source": source_type,
            "sources": sources_out,
            "invalidation": invalidation,
            "in_backoff": in_backoff,
            "disabled": disabled,
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

fn handle_introspect_config(config: &Config, registry: &ProviderRegistry) -> Response {
    let path_json = match config.loaded_path() {
        Some(p) => serde_json::Value::String(p.display().to_string()),
        None => serde_json::Value::Null,
    };
    let source_count = registry.config_source_count() as u64;
    let verdicts = vec![
        serde_json::json!({"level": "PASS", "message": "config parsed"}),
        serde_json::json!({"level": "PASS", "message": format!("{source_count} config Sources loaded")}),
    ];
    Response::ok(
        serde_json::json!({
            "path": path_json,
            "parsed": true,
            "errors": [],
            "config_source_count": source_count,
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
                "message": format!("{label} — stage={} elapsed={}", entry.stage, secs(entry.elapsed_secs)),
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
                "message": format!("{label} overdue (interval={}, last={} ago)", secs(t.interval_secs), secs(t.last_run_secs_ago)),
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

/// A count of seconds, as every duration comb shows reads.
fn secs(n: u64) -> String {
    libbeachcomber::render::duration(std::time::Duration::from_secs(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A read that finds a Source's instance cold while a call is already
    /// running it waits for that run and reads what it stored, rather than
    /// running the Source a second time beside it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_read_waits_for_the_run_that_has_its_instance() {
        use crate::cache::{Cache, Schedule};
        use crate::provider::Value;
        use std::collections::HashMap;

        let dir = tempfile::tempdir().unwrap();
        let runs = dir.path().join("runs");
        let registry = ProviderRegistry::with_config(
            &crate::config_tests::with_shipped_defaults(&format!(
                "[daemon.providers.demo.sources.s]\n\
                 data = \"{{{{ exec(['sh', '-c', 'echo run >> {}; echo 1']) | trim }}}}\"\n\
                 result.v = \"{{{{ data }}}}\"\n",
                runs.display()
            )),
            &Default::default(),
        )
        .unwrap();
        let cache = Cache::new();

        // Another caller is running the instance: it holds its turn.
        let turn = registry.cold_lock("demo", "s", None);
        let running = turn.last.lock().await;

        let mut warm = Box::pin(warm_source(
            &registry,
            &cache,
            "demo",
            "s",
            None,
            Warm::Cold,
        ));
        tokio::select! {
            biased;
            done = &mut warm => panic!("the read ran the Source beside the other run: {done:?}"),
            () = tokio::task::yield_now() => {}
        }

        // That run finishes and stores its result, as a call does.
        cache.put_source(
            "demo",
            None,
            "s",
            HashMap::from([("v".to_string(), Value::Int(1))]),
            Schedule::default(),
        );
        turn.finished
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        drop(running);

        warm.await.expect("the read goes on");
        assert!(
            !runs.exists(),
            "the Source ran once, in the other caller: the read found it stored"
        );
    }

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

        let registry = ProviderRegistry::with_defaults();
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
            .write_static("p", None, "put", "x", Value::Int(1), Some(1))
            .expect("put accepted");
        tokio::time::advance(std::time::Duration::from_secs(5)).await;
        cache
            .write_static(
                "p",
                None,
                "config:t.toml",
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
                None,
            )
            .expect("put accepted");
        tokio::time::advance(std::time::Duration::from_secs(9)).await;
        // The near layer: a directory below it, filling that node in.
        cache
            .write_static("p", Some("/x/y"), "put", "tree.left", Value::Int(1), None)
            .expect("put accepted");

        let read = cache
            .read_merged("p", Some("/x/y"), &[], &registry, crate::cache::Layers::All)
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
