use crate::boundaries::proc_snap::{ProcessSnapshotter, RealProcessSnapshotter};
use crate::cache::{Cache, Layers, MergedRead, RecordState};
use crate::config::Config;
use crate::protocol::{IntrospectSubject, Request, Response, Wait};
use crate::provider::registry::{ProviderRegistry, RegistryHandle};
use crate::provider::source_call::instance_key;
use crate::query::{Ask, Found, Instance, InstanceWalk, KeyParse, QueryPlan};
use crate::run::{Outcome, Want};
use crate::scheduler::{
    DemandInfo, LifecycleInfo, PollTimerInfo, SchedulerHandle, SchedulerMessage,
};
use crate::watcher_registry::WatcherRegistry;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tracing::{debug, info, warn};

/// How long the accept loop waits after a failed accept. Out of descriptors, a
/// pending connection stays readable (on Linux; macOS drops it), so without a
/// wait the next try fails at once, for as long as none is free.
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

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
    /// What `introspect procs` samples process starts through: the system's
    /// tracer, or a test's stand-in ([`Self::with_process_snapshotter`]).
    procs: Arc<dyn ProcessSnapshotter>,
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
            procs: Arc::new(RealProcessSnapshotter),
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
            procs: Arc::new(RealProcessSnapshotter),
        }
    }

    /// Attach reaper health state for introspect surfacing (daemon path only).
    pub fn with_reaper_health(mut self, health: Arc<crate::singleton::ReaperHealth>) -> Self {
        self.reaper = Some(health);
        self
    }

    /// Sample process starts for `introspect procs` through `procs` instead
    /// of the system's tracer.
    pub fn with_process_snapshotter(mut self, procs: Arc<dyn ProcessSnapshotter>) -> Self {
        self.procs = procs;
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

    /// Accept loop over a listener from [`Server::bind`]. An accept that fails
    /// is tried again every [`ACCEPT_RETRY`]; a run of failures logs once.
    pub async fn serve(&self, listener: UnixListener) -> std::io::Result<()> {
        let mut failing = false;
        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    if std::mem::take(&mut failing) {
                        info!("Accepting connections again");
                    }
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
                    let procs = Arc::clone(&self.procs);
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
                            procs,
                        )
                        .await
                        {
                            debug!("Connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    if !std::mem::replace(&mut failing, true) {
                        warn!(
                            "Accept error: {e}; trying again every {ACCEPT_RETRY:?} until one works"
                        );
                    }
                    tokio::time::sleep(ACCEPT_RETRY).await;
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
    procs: Arc<dyn ProcessSnapshotter>,
) -> std::io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

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
                    start_instant,
                    &requests_total,
                    &active_watches,
                    &socket_path,
                    &config,
                    reaper.as_deref(),
                    &procs,
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
/// ([`ProviderRegistry::instance_root`]), and whether its line was rendered to
/// find it, without holding an async worker for a render: a config Source's
/// `root` line can run a command (`exec`, up to its timeout), fetch a URL or
/// call a plugin. The registry keeps each rendered root per directory while
/// the directories on its walk are as they were (one `stat` each, here), so
/// only the first request for a directory, and the first after a marker on
/// its walk came or went, pays for the render, on the blocking pool; a kept
/// root, and a test Source's own, resolve in place.
async fn root_for(
    registry: &Arc<ProviderRegistry>,
    provider: &str,
    source: &str,
    dir: &str,
) -> Result<(Found, bool), String> {
    if registry.renders_root(provider, source) {
        if let Some(kept) = registry.kept_root(provider, source, dir) {
            return Ok((kept, false));
        }
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
        .map_err(|e| format!("resolving the root failed: {e}"))?
        .map(|found| (found, true));
    }
    registry
        .instance_root(provider, source, dir)
        .map(|found| (found, false))
}

/// The roots found so far by one request, so Sources that find theirs the same
/// way ([`crate::provider::Source::root_key`]) ask once per directory.
#[derive(Default)]
struct RootMemo {
    known: HashMap<(String, String, String), Found>,
    /// Whether any root was rendered, not answered from a kept one.
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
                    let (found, rendered) = root_for(registry, provider, source, &dir).await?;
                    memo.rendered |= rendered;
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
    cache: &Arc<Cache>,
    handle: &RegistryHandle,
    scheduler: Option<&SchedulerHandle>,
    watchers: &Arc<WatcherRegistry>,
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
) {
    let requested = path.as_deref();
    // A reload puts another registry in force; the watch's plan was made under
    // the one before it, so it is made again. Subscribed before the first plan
    // is made, so a reload during it is not missed.
    let mut swapped = handle.subscribe();
    let registry = swapped.borrow_and_update().clone();
    let mut setup = match watch_setup(&key, requested, registry, scheduler, watchers).await {
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

    // Every half decides whether a frame is new. A template edited on its own
    // changes what the client renders just as much as a value does, and so does
    // a memo whose hash an invalidation dropped, so a frame whose only change
    // is one of those is still a frame.
    let mut last = frame_identity(&initial);

    // Stream loop. A watch owns its connection for its whole lifetime — after
    // the initial request the client never writes again — so any completion
    // of the read races the write-triggered path: `Ok(0)` EOF, an error, or
    // even `Ok(n>0)` (a protocol-violating extra write) all mean this
    // connection is done being useful, and the watch ends. This is what lets
    // a dead client's watch drop at once instead of lingering until the next
    // write to the watched key. ("At once" has one hole: a `sched.send` into
    // a full scheduler queue parks this task for up to `SCHEDULER_WAIT`.)
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
                setup = match watch_setup(&key, requested, registry, scheduler, watchers).await {
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

        // Field-level filtering: skip if no half changed.
        let current = frame_identity(&response);
        if current == last {
            continue;
        }
        last = current;

        if write_watch_line(writer, &response).await.is_err() {
            break; // Client disconnected
        }
    }
}

/// What decides whether a watch frame is new: its data, its template and its
/// memo half. Ages are not compared: a frame is about what changed, and an
/// age always has.
type FrameIdentity = (
    Option<serde_json::Value>,
    Option<serde_json::Value>,
    Option<serde_json::Value>,
);

fn frame_identity(frame: &Response) -> FrameIdentity {
    (
        frame.data.clone(),
        frame.template.clone(),
        frame
            .memo
            .as_ref()
            .map(|memo| libbeachcomber::protocol::memo_without_ages(frame.template.as_ref(), memo)),
    )
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

/// Make a watch's [`WatchSetup`] under `registry` and signal demand for what
/// it reads. The error is for the client.
async fn watch_setup(
    key: &str,
    requested: Option<&str>,
    registry: Arc<ProviderRegistry>,
    scheduler: Option<&SchedulerHandle>,
    watchers: &Arc<WatcherRegistry>,
) -> Result<WatchSetup, String> {
    // The plan refuses a facet outside the closed set, as for every op.
    let plan = QueryPlan::build(key, requested, &registry)?;
    let provider_name = plan.provider.clone();

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

/// What a read asks of each instance it reaches.
#[derive(Clone, Copy, Debug)]
struct Read {
    /// `--force`: a run that begins now, past the failback backoff.
    force: bool,
    /// How long the read waits for each run it needs.
    wait: Wait,
    /// When the read began: every wait counts from here.
    began: tokio::time::Instant,
}

impl Read {
    fn new(force: bool, wait: Wait) -> Self {
        Self {
            force,
            wait,
            began: tokio::time::Instant::now(),
        }
    }

    /// Until when the read waits for a run, given what it waits for when the
    /// call says nothing (`budget`: `Some` for a record it has, `None`, the
    /// run's end, for a cold one or a forced run); `None` is to the run's
    /// end.
    fn until(&self, budget: Option<Duration>) -> Option<tokio::time::Instant> {
        match self.wait {
            Wait::Budget => budget.map(|budget| self.began + budget),
            Wait::For(wait) => Some(self.began + wait),
            Wait::Forever => None,
        }
    }
}

/// A run a read started or took, and how long it waits for it: `None` is to
/// its end.
struct Pending {
    run: crate::run::Run,
    until: Option<tokio::time::Instant>,
}

/// Start the run, or take the one in flight, that one instance of a Source
/// needs for `read`, and say how long the read waits for it. `None` when it
/// needs none: its record is valid (within its lifetime, its stat inputs as
/// its run found them), or it is in failback backoff (only `--force` runs past
/// that). `root` is the instance: the cache entry its records live at.
///
/// | The read finds | It runs | It waits for |
/// |---|---|---|
/// | a valid record | nothing | nothing |
/// | a record whose inputs changed | the run in flight if it sees them as they are, or one | the call's `wait`, else the Source's budget |
/// | a record past its lifetime (`stale`) | the run in flight, or one | as above |
/// | nothing stored | the run in flight, or one | the call's `wait`, else the run's end |
/// | anything, with `--force` | one that begins now | as above |
///
/// The check stats the record's inputs on the request task: one `stat` per
/// input, bounded by their count, not by time.
///
/// The decision is per instance, never per provider: a warm sibling says
/// nothing about this one. A run replaces the instance's map when it succeeds
/// and leaves it when it fails, so the static records sharing the entry, and
/// the last good value, stand either way.
fn start_for_read(
    registry: &Arc<ProviderRegistry>,
    cache: &Arc<Cache>,
    provider: &str,
    source_name: &str,
    root: Option<&str>,
    read: Read,
) -> Option<Pending> {
    let source = registry.source(provider, source_name)?;
    let budget = Some(source.metadata().budget);
    let (want, until) = if read.force {
        (Want::Forced, read.until(None))
    } else {
        // A valid instance is the common read: answered before anything
        // shared is locked.
        match cache.record_state(provider, root, source_name) {
            RecordState::Valid => return None,
            RecordState::Cold => (Want::Cold, read.until(None)),
            RecordState::Invalid => (Want::Changed, read.until(budget)),
            RecordState::Stale => (Want::Any, read.until(budget)),
        }
    };
    let key = instance_key(provider, source_name, root);
    let run = registry.runs().start(want, registry, cache, &key)?;
    Some(Pending { run, until })
}

/// A read of an instance whose Source watches its tree waits until the file
/// events from before it have arrived: a change in the tree, which no stat
/// sees, is then in the record's state for [`start_for_read`] to find.
async fn hear_tree(
    registry: &Arc<ProviderRegistry>,
    cache: &Arc<Cache>,
    provider: &str,
    source_name: &str,
    root: Option<&str>,
    read: Read,
) {
    if read.force {
        return;
    }
    let Some(source) = registry.source(provider, source_name) else {
        return;
    };
    if crate::provider::watches_tree(&source.metadata().invalidation)
        && cache.tree_is_watched(provider, root, source_name)
        && let Some(barrier) = cache.barrier()
    {
        barrier.pass(read.began).await;
    }
}

/// Wait for each of `pending`, all under way together, until its deadline:
/// whether any ended in time. A run that is not over by then goes on and
/// records itself; the read answers with what is stored.
///
/// A run that failed is a miss (its failure is logged where it is recorded),
/// and the read goes on to the rest of the chain. One that failed because a
/// Source it calls failed, or called itself, is a fault in the lines, not in
/// the world: the read shows it, as it shows a `root` that fails.
async fn settle(pending: Vec<Pending>) -> Result<bool, String> {
    let mut ended = false;
    for Pending { run, until } in pending {
        let outcome = match until {
            None => Some(run.outcome().await),
            Some(deadline) => run.outcome_by(deadline).await,
        };
        match outcome {
            Some(Outcome::Failed {
                message,
                fault: true,
            }) => return Err(message),
            Some(_) => ended = true,
            None => {}
        }
    }
    Ok(ended)
}

/// Start the run of an instance that `want` needs, or take the one in flight
/// (whoever started it), and wait for it: whether there was one to wait for.
/// An instance in failback backoff has none unless `want` is forced.
///
/// A run that failed is a miss (its failure is logged where it is recorded),
/// and the read goes on to the rest of the chain. One that failed because a
/// Source it calls failed, or called itself, is a fault in the lines, not in
/// the world: the read shows it, as it shows a `root` that fails.
///
/// The run records itself: a read that stops waiting (its connection gone)
/// takes nothing with it.
async fn run_for_read(
    registry: &Arc<ProviderRegistry>,
    cache: &Arc<Cache>,
    want: Want,
    provider: &str,
    source_name: &str,
    root: Option<&str>,
) -> Result<bool, String> {
    let key = instance_key(provider, source_name, root);
    let Some(run) = registry.runs().start(want, registry, cache, &key) else {
        return Ok(false);
    };
    match run.outcome().await {
        Outcome::Failed {
            message,
            fault: true,
        } => Err(message),
        _ => Ok(true),
    }
}

/// The instances, by provider, of the Sources `provider`'s `result` and
/// `return` lines call, as a read from `dir` reaches them, and whether a `root`
/// was rendered to find them, not answered from a kept one. A `root` that
/// needs rendering is rendered here, off the async workers.
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

/// Bring up to date, as `read` asks, every Source of another provider that
/// `provider`'s `result` and `return` lines call, at each instance a read from
/// `dir` reaches (their runs started together, then waited for), and say
/// which those were and whether the read must be made again: a run ended, or
/// a root was resolved that the first read did not have. What the lines then
/// render over is stored. These calls are part of the read: its budget
/// applies to them.
async fn warm_called(
    registry: &Arc<ProviderRegistry>,
    cache: &Arc<Cache>,
    provider: &str,
    dir: Option<&str>,
    read: Read,
) -> Result<(Vec<(String, Vec<Instance>)>, bool), String> {
    let (called, resolved) = called_instances(registry, provider, dir).await?;
    for (called_provider, instances) in &called {
        for instance in instances {
            let root = instance.root.as_deref();
            hear_tree(
                registry,
                cache,
                called_provider,
                &instance.source,
                root,
                read,
            )
            .await;
        }
    }
    let pending: Vec<Pending> = called
        .iter()
        .flat_map(|(called_provider, instances)| {
            instances.iter().filter_map(move |instance| {
                start_for_read(
                    registry,
                    cache,
                    called_provider,
                    &instance.source,
                    instance.root.as_deref(),
                    read,
                )
            })
        })
        .collect();
    let ended = settle(pending).await?;
    Ok((called, resolved || ended))
}

/// What a climb does at each instance it reaches.
#[derive(Clone, Copy)]
enum Climb {
    /// Bring the instance up to date as the read asks, then read.
    Warm(Read),
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
/// Each instance's run starts as the climb reaches it, and the read waits for
/// the runs of a level together before it tries the merged read there. A read
/// that cannot stop early (a namespace, or a provider whose lines decide what
/// exists) starts every run it needs before it waits for any.
///
/// `honor_put` lets a field a `put` states at an instance be answered there,
/// without running the Source it hides.
async fn climb(
    registry: &Arc<ProviderRegistry>,
    cache: &Arc<Cache>,
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
    let stops_early = by_default && matches!(plan.target, KeyParse::Field(..));
    let mut pending = Vec::new();
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
            if !pending.is_empty() || out.merged.is_none() {
                settle(std::mem::take(&mut pending)).await?;
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
            if let Climb::Warm(read) = mode {
                hear_tree(registry, cache, provider, &w.name, root.as_deref(), read).await;
                pending.extend(start_for_read(
                    registry,
                    cache,
                    provider,
                    &w.name,
                    root.as_deref(),
                    read,
                ));
            }
            out.reached.push(Instance {
                source: w.name.clone(),
                root,
            });
        }
        if stops_early {
            settle(std::mem::take(&mut pending)).await?;
            out.merged = read_chain_merged(cache, registry, plan, &out.visited);
            if answers_field(&out.merged, &plan.target) {
                return Ok(out);
            }
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
fn hit_response(data: serde_json::Value, freshness: Option<u128>) -> Response {
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
/// A leaf is answered by its own record's source entry — canon
/// `provider_source.md` §"Field freshness". An object is answered by the
/// records under it together: the largest of their ages. `None` when no record sits at or below the address, which is
/// also when there is no `data` to report freshness for.
fn freshness(read: &MergedRead, rest: &str) -> Option<u128> {
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
struct TemplateHalf {
    /// The node at the requested address in the `template` facet's merged tree.
    template: Option<serde_json::Value>,
    /// The part of the address the walk could not consume.
    rest: Option<String>,
    /// The address `template` stands at: the requested one, or that of the
    /// string the walk stopped on.
    at: String,
    /// The memos of the template's leaves ([`memo_half`]).
    memo: Option<serde_json::Value>,
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
fn memo_half(
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
fn resolve_halves(merged: &Option<MergedRead>, target: &KeyParse, half: TemplateHalf) -> Response {
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
    resolve_halves(&merged, target, half)
}

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
            Some(value) => Response::ok(value, 0),
            None => Response::miss(),
        };
    }

    // A key below a leaf (`p:cache.f.age_ms`) reads into that leaf's answer:
    // the deepest node of the key the read reaches answers, and the rest of
    // the key walks into that answer.
    let segments = libbeachcomber::key::segments(rest);
    let answered = (segments.len().min(1)..=segments.len())
        .rev()
        .find_map(|n| {
            let node = libbeachcomber::key::format_address(&segments[..n]);
            let tree = cache_facet_tree(cache, registry, provider, &node, requested, reached)?;
            Some((tree, libbeachcomber::key::format_address(&segments[n..])))
        });
    match answered {
        Some((tree, left)) if left.is_empty() => Response::ok(tree, 0),
        Some((tree, left)) => libbeachcomber::resolve::walk(tree, &left)
            .map_or_else(Response::miss, |v| Response::ok(v, 0)),
        None => Response::miss(),
    }
}

/// The `:cache` tree at `rest`, or `None` when the read does not reach a node
/// there ([`cache_facet_response`]).
fn cache_facet_tree(
    cache: &Cache,
    registry: &ProviderRegistry,
    provider: &str,
    rest: &str,
    requested: Option<&str>,
    reached: &[Instance],
) -> Option<serde_json::Value> {
    let merged = cache.read_merged(provider, requested, reached, registry, Layers::All);
    let Answered { merged, half } = answer(cache, registry, provider, rest, requested, merged);
    let value = merged.as_ref().and_then(|m| {
        if rest.is_empty() {
            Some(crate::provider::Value::Object(m.fields.clone()))
        } else {
            crate::provider::lookup_path(&m.fields, rest).cloned()
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
        let label: &str = &record.source;
        answer.insert("source".into(), crate::cache::presented_label(label).into());
        // A Source's record is being brought up to date while a run of its
        // instance is in flight, whoever started it.
        if !crate::cache::is_static_label(label) {
            let instance = instance_key(provider, label, record.entry.as_deref());
            answer.insert(
                "executing".into(),
                registry.runs().is_running(&instance).into(),
            );
        }
        let stored = cache.stored_keys(provider, record.entry.as_deref(), label, tail);
        for (key, v) in stored.iter().flatten() {
            if let Ok(v) = serde_json::to_value(v) {
                answer.insert(key.clone(), v);
            }
        }
        if !crate::cache::is_static_label(label) {
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
fn cache_facet_node(
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

/// A request's `ttl`: whole seconds and at least one, or none. One reading
/// for a `put` and a `memo`.
fn parse_ttl(ttl: Option<&str>) -> Result<Option<u64>, String> {
    let Some(t) = ttl else {
        return Ok(None);
    };
    match crate::config::parse_whole_seconds(t).map(|d| d.as_secs()) {
        None => Err(format!("invalid ttl: {t}")),
        Some(0) => Err("ttl must be at least 1s".to_string()),
        Some(n) => Ok(Some(n)),
    }
}

/// What a `memo` request writes, checked: the provider, the directory, the
/// leaf's canonical address and the memo; or why it is refused. The key is
/// a provider's field, never a facet or a bare provider; the hash sixteen
/// lowercase hex digits; each watch path absolute; the ttl as a `put`'s.
fn memo_write(
    key: &str,
    path: Option<&str>,
    hash: &str,
    data: Option<&serde_json::Value>,
    watch: &[String],
    ttl: Option<&str>,
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
    let path = crate::query::normalise_path(path)?;
    let write = crate::cache::MemoWrite {
        data: data.and_then(crate::provider::Value::from_json),
        hash: hash.to_string(),
        watch: watch.to_vec(),
        ttl_secs,
    };
    Ok((provider.to_string(), path, field, write))
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
    cache: &Arc<Cache>,
    registry: &Arc<ProviderRegistry>,
    scheduler: Option<&SchedulerHandle>,
    start_instant: Instant,
    requests_total: &AtomicU64,
    active_watches: &AtomicUsize,
    socket_path: &std::path::Path,
    config: &Config,
    reaper: Option<&crate::singleton::ReaperHealth>,
    procs: &Arc<dyn ProcessSnapshotter>,
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
            let requested = path.as_deref();
            let plan = match QueryPlan::build(key, requested, registry) {
                Ok(plan) => plan,
                Err(msg) => return Response::error(msg),
            };
            let provider_name = plan.provider.as_str();

            // The plan has refused a facet outside the closed set; what is
            // left on the provider segment is the template facet or none.
            let facet = crate::query::parse_facet(provider_name).ok().flatten();

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

            // Every Source the key demands is brought up to date on its own
            // terms — one cold source is no longer covered for by a warm
            // sibling — and each at its instances, nearest first. `plan.demand`
            // is every Source for a bare namespace and the owning Source for a
            // field, which is what makes this one loop serve both — and what
            // makes it the same set `refresh` re-executes. The `put` layer is
            // what `put` wrote: no Source has anything to run for it.
            let demanded =
                crate::query::demanded_source_names(registry, provider_name, &read_demand(&plan));
            // How long the read waits for each run it needs: its budgets
            // count from now.
            let read = Read::new(*force, *wait);

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
                Climb::Warm(read),
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
                    read,
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
            // `--force` computes the value afresh: a Source re-executes, and
            // a template renders past its memo on the client. A static
            // namespace has no Source, so only a template half gives a forced
            // read something to do.
            if *force && half.template.is_none() && registry.is_static_namespace(provider_name) {
                return Response::error(format!(
                    "cannot --force static namespace '{provider_name}': no source to re-execute from"
                ));
            }
            resolve_halves(&merged, &plan.target, half)
        }
        Request::Refresh { key, path } => {
            // The same plan every other op builds, so the key is checked and
            // the path resolved exactly as for a `get`.
            let requested = path.as_deref();
            let plan = match QueryPlan::build(key, requested, registry) {
                Ok(plan) => plan,
                Err(msg) => return Response::error(msg),
            };
            let provider_name = plan.provider.as_str();
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
            // A cached template under the key renders afresh at its next read,
            // wherever its memos are: their hashes go. That is a refresh too.
            let memos = cache.drop_memo_hashes_under(provider_name, plan.target.rest());
            if demanded.is_empty() {
                if memos {
                    return Response::miss();
                }
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
                if memos {
                    return Response::miss();
                }
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
                // No scheduler (an embedded server): the same runs, a run of
                // each that begins after now, waited for here.
                None => {
                    for instance in &instances {
                        let warmed = run_for_read(
                            registry,
                            cache,
                            Want::Fresh,
                            provider_name,
                            &instance.source,
                            instance.root.as_deref(),
                        )
                        .await;
                        if let Err(msg) = warmed {
                            warn!("refresh {key}: {msg}");
                            return Response::error(msg);
                        }
                    }
                }
            }
            Response::miss()
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

            // A template is linted as a configured one is: one that does not
            // compile is refused. A name no provider has is not, since a later
            // put may make that provider.
            if facet == Some(Facet::Template) {
                let engine = libbeachcomber::eval::Probe::new(
                    libbeachcomber::engine::build_engine_env(Default::default()),
                );
                let at = libbeachcomber::key::join(namespace, node);
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
            let ttl_secs = match parse_ttl(ttl.as_deref()) {
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
            if data.is_null() {
                cache.remove_put(namespace, effective_path.as_deref(), node);
                if let Some(sched) = scheduler {
                    sched.send(put_written()).await;
                }
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

            Response::miss()
        }
        Request::Memo {
            key,
            path,
            hash,
            data,
            watch,
            ttl,
        } => match memo_write(
            key,
            path.as_deref(),
            hash,
            data.as_ref(),
            watch,
            ttl.as_deref(),
        ) {
            Ok((provider, path, field, write)) => {
                let written = crate::scheduler::SchedulerMessage::MemoWritten {
                    provider: provider.clone(),
                    path: path.clone(),
                    node: field.clone(),
                    ttl_secs: write.ttl_secs,
                    watch: write.watch.clone(),
                };
                cache.write_memo(&provider, path.as_deref(), &field, write);
                if let Some(sched) = scheduler {
                    sched.send(written).await;
                }
                Response::miss()
            }
            Err(msg) => Response::error(msg),
        },
        Request::Status {} => {
            let mut rows = cache.list_rows();

            let (lifecycle, failures) = if let Some(sched) = scheduler {
                let lc = sched.get_lifecycle_snapshots().await;
                let fs = sched.get_failure_states().await;
                (lc, fs)
            } else {
                (Default::default(), Default::default())
            };

            // A Source that has failed every run has stored nothing, so it
            // has no row: give it an absent one per field it declares, which
            // the loop below fills with its lifecycle and its failure. A
            // whole-node Source's fields are unknown until it runs; its one
            // row names its sentinel (`<node>`), as `introspect providers`
            // lists it.
            for key in failures.keys() {
                let has_row = rows.iter().any(|r| {
                    r.provider == key.provider && r.path == key.path && r.source == key.source
                });
                if !key.node.is_empty() || has_row {
                    continue;
                }
                let fields = registry
                    .source_metadata(&key.provider, &key.source)
                    .map(|sm| sm.fields.iter().map(|f| f.name.clone()).collect())
                    .unwrap_or_else(|| vec![String::new()]);
                for field in fields {
                    rows.push(crate::cache::CacheRow {
                        shadowed: false,
                        provider: key.provider.clone(),
                        path: key.path.clone(),
                        source: key.source.clone(),
                        field,
                        value: serde_json::Value::Null,
                        age_ms: None,
                        kind: None,
                        poll_interval_secs: None,
                        poll_count: None,
                        polls_elapsed: None,
                        next_poll_in_secs: None,
                        failure: None,
                    });
                }
            }

            use crate::cache::RowKind;
            for row in rows.iter_mut() {
                // `Cache::list_rows` already marked static rows (source ==
                // PUT_SOURCE or "config:*") with RowKind::Static; only rows
                // backed by a real Source need a lifecycle/transient lookup.
                // A put row is looked up only when its address carries a ttl
                // (`Cache::list_rows` leaves its kind unset); without one it
                // stays static, whatever put sits above it.
                let is_put = row.source == crate::cache::PUT_SOURCE;
                // A memo row is a `<p>:memo` row; its lifecycle is `<p>`'s,
                // at its own address.
                let is_memo = row.source == crate::cache::MEMO_SOURCE;

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
                        source: row.source.clone(),
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

            // The internal labels keyed the lookups above; a reader gets the
            // presented form, as a `:cache` answer does.
            for row in rows.iter_mut() {
                row.source = crate::cache::presented_label(&row.source);
            }

            match serde_json::to_value(&rows) {
                Ok(v) => Response::ok(v, 0),
                Err(e) => Response::error(format!("serialization failed: {e}")),
            }
        }
        Request::Introspect {
            subject,
            duration_secs,
        } => match subject {
            IntrospectSubject::Daemon => {
                // Gather in_flight, the watch backend and the watches that
                // failed from scheduler status if available.
                let (in_flight_count, watch_backend, watches) = if let Some(sched) = scheduler
                    && let Some(sched_status) = sched.get_status().await
                {
                    (
                        sched_status.in_flight.len() as u64,
                        sched_status.watch_backend,
                        sched_status.watches,
                    )
                } else {
                    (0, "unknown".to_string(), Vec::new())
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
                    &watches,
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
                let procs = Arc::clone(procs);
                tokio::task::spawn_blocking(move || handle_introspect_procs(procs.as_ref(), dur))
                    .await
                    .unwrap_or_else(|e| Response::error(format!("procs task panicked: {e}")))
            }
        },
        Request::Layers { key, path } => {
            let requested = path.as_deref();
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
        Request::Hello {} => {
            let data = serde_json::json!({
                "protocol_version": crate::protocol::PROTOCOL_VERSION,
                "daemon_version": env!("BEACHCOMBER_VERSION"),
            });
            Response::ok(data, 0)
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
    watches: &[crate::watcher::Watch],
    reaper: Option<&crate::singleton::ReaperHealth>,
    loaded_config: Option<&std::path::Path>,
) -> Response {
    use crate::protocol::{DaemonHealth, ReaperStatus, Verdict};
    let verdict = |level: &str, message: String| Verdict {
        level: level.to_string(),
        message,
    };

    let uptime_secs = start_instant.elapsed().as_secs();

    let mut verdicts = vec![verdict("PASS", "daemon responsive".to_string())];

    if in_flight_count > 50 {
        verdicts.push(verdict(
            "WARN",
            format!("{in_flight_count} in-flight requests (threshold 50)"),
        ));
    } else {
        verdicts.push(verdict("PASS", format!("in_flight={in_flight_count}")));
    }

    // Canon provider_source.md invariant 16: watch degradation is observable.
    if watch_backend == "native" {
        verdicts.push(verdict(
            "PASS",
            "watch backend: native fs events".to_string(),
        ));
    } else {
        verdicts.push(verdict("WARN", format!(
                "watch backend: {watch_backend} — kernel fs events undelivered; watch invalidation degraded to polling"
            )));
    }
    for failed in failed_watches(watches) {
        verdicts.push(verdict("WARN", failed));
    }

    // Canon singleton.md invariant 13: reaper capability is never silently
    // degraded. `reaper` is None for embedded/test servers (field omitted).
    let reaper = reaper.map(|health| {
        use std::sync::atomic::Ordering::Relaxed;
        let armed = health.armed.load(Relaxed);
        let visibility_ok = health.visibility_ok.load(Relaxed);
        let kill_denied = health.kill_denied_total.load(Relaxed);

        if !armed {
            verdicts.push(verdict("PASS", "reaper: not armed (side daemon)".to_string()));
        } else if visibility_ok {
            verdicts.push(verdict("PASS", "reaper: armed, system-wide process visibility".to_string()));
        } else {
            verdicts.push(verdict("WARN", "reaper visibility degraded: PID 1 not visible in process enumeration — this daemon runs confined (sandbox-spawned?) and cannot police daemons outside its confinement; restart it from an unconfined shell".to_string()));
        }
        if armed && kill_denied > 0 {
            verdicts.push(verdict("WARN", format!(
                    "reaper: {kill_denied} kill attempt(s) denied by the OS — orphans visible but unsignalable"
                )));
        }

        ReaperStatus {
            armed,
            visibility: if visibility_ok { "system-wide" } else { "confined" }.to_string(),
            sweeps: health.sweeps_total.load(Relaxed),
            reaped: health.reaped_total.load(Relaxed),
            kill_denied,
        }
    });

    let health = DaemonHealth {
        pid: i64::from(std::process::id()),
        version: env!("BEACHCOMBER_VERSION").to_string(),
        uptime_secs,
        socket_path: socket_path.to_string_lossy().into_owned(),
        config_path: loaded_config.map(|p| p.to_string_lossy().into_owned()),
        requests_total: requests_total.load(Ordering::Relaxed),
        in_flight: in_flight_count,
        active_watchers,
        cache_entries,
        watch_backend: watch_backend.to_string(),
        reaper,
        verdicts,
    };

    Response::ok(
        serde_json::to_value(health).expect("daemon health serialises"),
        0,
    )
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
    let failures = match scheduler {
        Some(s) => s.get_failure_states().await,
        None => Default::default(),
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
            "source"
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
                        .map(|f| serde_json::json!({"name": f.name}))
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

        // A Source whose latest runs failed, whether or not one ever succeeded.
        // One verdict per Source: the instance with the longest streak.
        let mut failing: Vec<(&str, u32, Option<&str>)> = failures
            .iter()
            .filter(|(k, _)| &k.provider == name)
            .map(|(k, f)| {
                (
                    k.source.as_str(),
                    f.consecutive_failures,
                    f.last_error.as_deref(),
                )
            })
            .collect();
        failing.sort_by(|a, b| a.0.cmp(b.0).then(b.1.cmp(&a.1)));
        failing.dedup_by_key(|(source, _, _)| *source);
        for (source, n, error) in &failing {
            let why = error.map_or(String::new(), |e| format!(": {e}"));
            verdicts.push(serde_json::json!({
                "level": "WARN",
                "message": format!("{name}.{source} failing, {n} run(s) in a row{why}"),
            }));
        }
        let failing = (!failing.is_empty()).then(|| {
            serde_json::json!({
                "sources": failing.iter().map(|(s, _, _)| s).collect::<Vec<_>>(),
                "consecutive_failures": failing.iter().map(|(_, n, _)| n).max(),
            })
        });

        providers_out.push(serde_json::json!({
            "name": name,
            "failing": failing,
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
    )
}

fn handle_introspect_cache(cache: &Cache) -> Response {
    let entries = cache.list_entries();
    let total = entries.len() as u64;
    let verdicts = vec![serde_json::json!({
        "level": "PASS",
        "message": format!("{total} entries"),
    })];

    Response::ok(
        serde_json::json!({
            "total_entries": total,
            "verdicts": verdicts,
        }),
        0,
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
            // A static address is named by its node: `kubecontext.context`.
            let mut label = entry.provider.clone();
            if !entry.node.is_empty() {
                label = format!("{label}.{}", entry.node);
            }
            if let Some(p) = &entry.path {
                label = format!("{label} ({p})");
            }
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
    )
}

async fn handle_introspect_watches(scheduler: Option<&SchedulerHandle>) -> Response {
    let watches = match scheduler {
        Some(s) => s
            .get_status()
            .await
            .map(|st| st.watches)
            .unwrap_or_default(),
        None => Vec::new(),
    };

    let verdicts = watch_verdicts(&watches);

    Response::ok(
        serde_json::json!({
            "watches": watches,
            "verdicts": verdicts,
        }),
        0,
    )
}

/// How long a path may be `pending` before the worker is called slow: a
/// registration can take seconds, and a few are normal.
const PENDING_TOO_LONG_SECS: u64 = 30;

/// What a WARN verdict says of each watch whose registration failed. The next
/// demand of an instance that watches it asks for it again.
fn failed_watches(watches: &[crate::watcher::Watch]) -> impl Iterator<Item = String> + '_ {
    watches.iter().filter_map(|w| match &w.state {
        crate::watcher::WatchState::Failed { error } => Some(format!(
            "watch of {} failed: {error}; the next read of a Source that watches it tries again",
            w.path.display()
        )),
        _ => None,
    })
}

/// What a WARN verdict says of each watch pending past [`PENDING_TOO_LONG_SECS`].
fn slow_watches(watches: &[crate::watcher::Watch]) -> impl Iterator<Item = String> + '_ {
    watches.iter().filter_map(|w| match &w.state {
        crate::watcher::WatchState::Pending if w.asked_secs_ago >= PENDING_TOO_LONG_SECS => {
            Some(format!(
                "watch of {} still registering after {}",
                w.path.display(),
                secs(w.asked_secs_ago)
            ))
        }
        _ => None,
    })
}

/// The verdicts on the watch table, which holds a path for each watch an
/// Active instance declares and none otherwise (a watch exists only while its
/// instance is Active). So an empty table is an idle daemon, healthy; the
/// warnings are a declared watch that is not registered: one that failed, and
/// one still pending after [`PENDING_TOO_LONG_SECS`].
fn watch_verdicts(watches: &[crate::watcher::Watch]) -> Vec<serde_json::Value> {
    let mut verdicts = vec![if watches.is_empty() {
        serde_json::json!({
            "level": "PASS",
            "message": "idle: no instance is Active, so no path is watched",
        })
    } else {
        serde_json::json!({"level": "PASS", "message": format!("watching {} paths", watches.len())})
    }];
    verdicts.extend(
        failed_watches(watches)
            .chain(slow_watches(watches))
            .map(|message| serde_json::json!({"level": "WARN", "message": message})),
    );
    verdicts
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
    )
}

fn handle_introspect_procs(procs: &dyn ProcessSnapshotter, duration_secs: Option<u64>) -> Response {
    let dur = duration_secs.unwrap_or(2);
    match procs.capture(dur) {
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
            )
        }
        // The machine cannot trace process starts (no eslogger privileges,
        // an unsupported platform): a missing capability, not comb failing.
        Err(e) => Response::ok(
            serde_json::json!({
                "duration_secs": dur,
                "samples": [],
                "replacement_suggestions": [],
                "verdicts": [{
                    "level": "INFO",
                    "message": format!("process sampling unavailable: {e}"),
                }],
            }),
            0,
        ),
    }
}

/// A count of seconds, as every duration comb shows reads.
fn secs(n: u64) -> String {
    libbeachcomber::render::duration(std::time::Duration::from_secs(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scheduler that has stopped draining its queue does not cost a request
    /// its connection: `get` and `watch` are answered from the cache with the
    /// queue full, one [`crate::scheduler::SCHEDULER_WAIT`] later. Parked on
    /// that send for good, each kept its descriptor after its client had gone.
    #[tokio::test(start_paused = true)]
    async fn a_request_is_answered_with_the_schedulers_queue_full() {
        use tokio::io::AsyncWriteExt;
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("sock");
        let (tx, _never_drained) = tokio::sync::mpsc::channel(1);
        let handle = SchedulerHandle::new(tx);
        handle
            .send(SchedulerMessage::Refresh {
                provider: "p".into(),
                instances: Vec::new(),
            })
            .await;
        let cache = Arc::new(Cache::new());
        let registry = ProviderRegistry::new().share(&cache);
        Server::new(
            socket.clone(),
            cache,
            registry,
            Some(handle),
            Arc::new(WatcherRegistry::new()),
        )
        .spawn()
        .unwrap();

        for request in [
            r#"{"op":"get","key":"hostname"}"#,
            r#"{"op":"watch","key":"hostname"}"#,
        ] {
            let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
            let (reader, mut writer) = stream.into_split();
            writer
                .write_all(format!("{request}\n").as_bytes())
                .await
                .unwrap();
            let mut answer = String::new();
            BufReader::new(reader).read_line(&mut answer).await.unwrap();
            assert!(answer.contains(r#""ok":true"#), "{request}: {answer}");
        }
    }

    fn watch(
        path: &str,
        state: crate::watcher::WatchState,
        asked_secs_ago: u64,
    ) -> crate::watcher::Watch {
        crate::watcher::Watch {
            path: path.into(),
            state,
            asked_secs_ago,
        }
    }

    fn levels(verdicts: &[serde_json::Value]) -> Vec<&str> {
        verdicts
            .iter()
            .map(|v| v["level"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn no_watches_is_an_idle_daemon_not_a_warning() {
        let verdicts = watch_verdicts(&[]);
        assert_eq!(levels(&verdicts), ["PASS"]);
        assert!(verdicts[0]["message"].as_str().unwrap().contains("idle"));
    }

    #[test]
    fn a_failed_watch_warns_and_a_registered_one_does_not() {
        use crate::watcher::WatchState::{Active, Failed};
        let verdicts = watch_verdicts(&[
            watch("/a", Active { via: None }, 900),
            watch(
                "/b",
                Failed {
                    error: "gone".into(),
                },
                1,
            ),
        ]);
        assert_eq!(levels(&verdicts), ["PASS", "WARN"]);
        assert!(verdicts[1]["message"].as_str().unwrap().contains("/b"));
    }

    #[test]
    fn a_pending_watch_warns_only_once_it_has_waited_too_long() {
        use crate::watcher::WatchState::Pending;
        assert_eq!(
            levels(&watch_verdicts(&[watch(
                "/a",
                Pending,
                PENDING_TOO_LONG_SECS - 1
            )])),
            ["PASS"]
        );
        assert_eq!(
            levels(&watch_verdicts(&[watch(
                "/a",
                Pending,
                PENDING_TOO_LONG_SECS
            )])),
            ["PASS", "WARN"]
        );
    }

    /// A config Source whose run appends to `<dir>/runs`, touches
    /// `<dir>/started`, then waits for `<dir>/go` and answers 1.
    fn gated(dir: &std::path::Path) -> (Arc<ProviderRegistry>, Arc<Cache>) {
        let cache = Arc::new(Cache::new());
        let registry = ProviderRegistry::with_config(
            &crate::config_tests::with_shipped_defaults(&format!(
                "[daemon.providers.demo.sources.s]\n\
                 data = \"{{{{ exec(['sh', '-c', 'echo run >> {d}/runs; touch {d}/started; \
                 while [ ! -f {d}/go ]; do sleep 0.01; done; echo 1']) | trim }}}}\"\n\
                 result.v = \"{{{{ data }}}}\"\n",
                d = dir.display()
            )),
            &std::env::vars().collect(),
        )
        .unwrap()
        .share(&cache);
        (registry, cache)
    }

    /// A read of the instance at `~` of `demo.s`, as a `get` makes it.
    async fn read_of(registry: &Arc<ProviderRegistry>, cache: &Arc<Cache>) -> Result<bool, String> {
        let pending = start_for_read(
            registry,
            cache,
            "demo",
            "s",
            None,
            Read::new(false, Wait::Budget),
        );
        settle(pending.into_iter().collect()).await
    }

    async fn until(what: &str, done: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "never: {what}");
            tokio::task::yield_now().await;
        }
    }

    /// A read that finds its instance cold while a run of it is under way
    /// (here a poll's) takes that run rather than running the Source beside it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_read_takes_the_run_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, cache) = gated(dir.path());
        let key = instance_key("demo", "s", None);
        let poll = registry
            .runs()
            .start(Want::Any, &registry, &cache, &key)
            .unwrap();
        until("the poll's run to begin", || {
            dir.path().join("started").exists()
        })
        .await;

        let read = tokio::spawn({
            let (registry, cache) = (registry.clone(), cache.clone());
            async move { read_of(&registry, &cache).await }
        });
        until("the read to take the poll's run", || {
            registry.runs().callers(&key) == Some(2)
        })
        .await;
        std::fs::write(dir.path().join("go"), "").unwrap();
        assert_eq!(read.await.unwrap(), Ok(true), "the read waited for a run");
        assert_eq!(poll.outcome().await, Outcome::Stored);
        let runs = std::fs::read_to_string(dir.path().join("runs")).unwrap();
        assert_eq!(runs.lines().count(), 1, "the Source ran once, for both");
    }

    /// A read that stops waiting part-way through the run it started (its
    /// connection gone, say) takes nothing with it: the run records itself.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_read_that_stops_waiting_takes_nothing_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, cache) = gated(dir.path());
        {
            let read = read_of(&registry, &cache);
            tokio::pin!(read);
            tokio::select! {
                done = &mut read => panic!("the gated run ended: {done:?}"),
                () = until("the read's run to begin", || dir.path().join("started").exists()) => {}
            }
        }
        std::fs::write(dir.path().join("go"), "").unwrap();
        until("the run to store its record", || {
            cache.has_provider_records_for("demo", None, "s")
        })
        .await;
        assert!(registry.runs().in_flight().is_empty());
    }

    /// The provider `g` of one Source `s`, whose run is held at its start
    /// until the test sends on its channel (as a process slow to spawn would
    /// hold it), and then answers 1.
    struct SlowStart(Arc<std::sync::Mutex<std::sync::mpsc::Receiver<()>>>);

    impl crate::provider::Source for SlowStart {
        fn metadata(&self) -> &crate::provider::SourceMetadata {
            static META: std::sync::OnceLock<crate::provider::SourceMetadata> =
                std::sync::OnceLock::new();
            META.get_or_init(|| crate::provider::SourceMetadata {
                name: "s".into(),
                fields: vec![crate::provider::FieldSchema { name: "n".into() }],
                invalidation: crate::provider::InvalidationStrategy::Poll {
                    interval_secs: 60,
                    count: 2,
                },
                failback: crate::provider::FailbackConfig {
                    reattempts: 3,
                    interval_secs: 3600,
                },
                decay: None,
                budget: Duration::from_secs(3600),
                timeout: Duration::from_secs(600),
                nearest: false,
            })
        }

        fn execute(&self, _: Option<&str>) -> Result<crate::provider::SourceResult, String> {
            self.0.lock().unwrap().recv().map_err(|e| e.to_string())?;
            let mut result = crate::provider::SourceResult::new();
            result.insert("n", crate::provider::Value::Int(1));
            Ok(result)
        }
    }

    impl SlowStart {
        fn sources(&self) -> Vec<Box<dyn crate::provider::Source>> {
            vec![Box::new(SlowStart(Arc::clone(&self.0)))]
        }
    }

    /// A `--wait 0` read starts the run it needs and is answered on its first
    /// poll, however long the run takes to get going: it waits on nothing,
    /// the run and the runtime's timer (which turns in whole milliseconds)
    /// alike. Nothing ended in time, so the read answers what is stored; the
    /// run goes on and records itself. One worker, so nothing turns the timer
    /// between the read's start and its poll.
    #[tokio::test]
    async fn a_wait_0_read_is_answered_at_once_with_its_run_held_at_its_start() {
        let (go, held) = std::sync::mpsc::channel();
        let mut registry = ProviderRegistry::new();
        registry
            .register_sources(
                "g",
                SlowStart(Arc::new(std::sync::Mutex::new(held))).sources(),
            )
            .unwrap();
        let (registry, cache) = (Arc::new(registry), Arc::new(Cache::new()));
        let read = Read::new(false, Wait::For(Duration::ZERO));
        let pending =
            start_for_read(&registry, &cache, "g", "s", None, read).expect("a cold instance runs");
        let run = pending.run.clone();
        let answered = tokio_test::task::spawn(settle(vec![pending])).poll();
        go.send(()).unwrap();
        assert_eq!(
            answered,
            std::task::Poll::Ready(Ok(false)),
            "answered at once, no run ended"
        );
        assert_eq!(run.outcome().await, Outcome::Stored);
    }

    /// A watch frame is new when its data, its template or its memo half
    /// changed, and not when only an age did: a memo's `age_ms` in a group's
    /// tree as much as in a string's object, and never a key of the group that
    /// happens to be called `age_ms`.
    #[test]
    fn a_frame_is_new_when_a_half_changed_but_not_when_an_age_did() {
        use serde_json::json;
        let frame = |template: serde_json::Value, memo: serde_json::Value| Response {
            template: Some(template),
            memo: Some(memo),
            ..Response::miss()
        };
        let leaf = json!("{{ 1 }}");
        let at = |age: u64, hash: Option<&str>| {
            let mut m = json!({"stale": false, "age_ms": age, "data": 1});
            if let Some(h) = hash {
                m["hash"] = json!(h);
            }
            m
        };
        assert_eq!(
            frame_identity(&frame(leaf.clone(), at(1, Some("a")))),
            frame_identity(&frame(leaf.clone(), at(900, Some("a"))))
        );
        assert_ne!(
            frame_identity(&frame(leaf.clone(), at(1, Some("a")))),
            frame_identity(&frame(leaf.clone(), at(1, None))),
            "a dropped hash"
        );
        let group = json!({"age_ms": "{{ 1 }}", "g": {"h": "{{ 2 }}"}});
        let tree =
            |age: u64, h: u64| json!({"age_ms": at(age, Some("a")), "g": {"h": at(h, Some("b"))}});
        assert_eq!(
            frame_identity(&frame(group.clone(), tree(1, 2))),
            frame_identity(&frame(group.clone(), tree(3, 4)))
        );
        assert_ne!(
            frame_identity(&frame(group.clone(), tree(1, 2))),
            frame_identity(&frame(group.clone(), json!({"g": {"h": at(2, Some("b"))}}))),
            "a group's leaf named age_ms is a leaf, not an age"
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
