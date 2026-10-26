use crate::boundaries::library::LibloadingLoader;
use crate::cache::{Cache, Layers};
use crate::config::Config;
use crate::engine::PluginSet;
use crate::provider::config_source::{ConfigSource, DaemonRead, lint_lines};
use crate::provider::source_call::Called;
use crate::provider::{
    FailbackConfig, Provider, ProviderMetadata, Source, SourceMetadata, SourceResult, Value,
    is_sentinel,
};
use crate::query::Found;
use crate::scheduler::lifecycle::{DEFAULT_FAILBACK_COUNT, DEFAULT_FAILBACK_INTERVAL_SECS};
use crate::shape::Retune;
use crate::shape::decorators::EngineDefaults;
use crate::shape::provider::ProviderLines;
use dashmap::DashMap;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock, Weak};

/// What [`ProviderRegistry::load`] made of a configuration.
pub struct ConfigLoad {
    /// The providers that did load; trustworthy only when `errors` is empty.
    pub registry: ProviderRegistry,
    pub errors: Vec<String>,
    /// What is likely a mistake but may not be, and does not stop the load:
    /// a client template reading a name no provider has, which a `put` may
    /// yet make.
    pub warnings: Vec<String>,
}

/// What a configuration declares: the config Sources, by provider, and the
/// built-in Sources a `return` retunes.
#[derive(Default)]
pub struct ConfigSources {
    pub providers: Vec<(String, Vec<ConfigSource>)>,
    pub retunes: Vec<Retune>,
    /// The plugin libraries the Sources' lines call, on the configured search
    /// path.
    pub plugins: Option<Arc<PluginSet>>,
}

/// The registry the daemon is serving from, and the one place it is swapped.
/// Whoever serves a request or handles a message takes [`Self::current`] once
/// and uses that registry throughout, so a reload in the middle cannot mix two.
///
/// Cloning gives another handle on the same slot; a bare registry converts to a
/// handle of its own, for an embedder or a test that never reloads. A `watch`
/// channel rather than an `arc-swap` dependency: a request pays one read lock
/// and one `Arc` clone, never one per lookup, and a long-lived reader (a
/// `watch` stream) can wait for the next registry with [`Self::subscribe`].
#[derive(Clone)]
pub struct RegistryHandle(Arc<tokio::sync::watch::Sender<Arc<ProviderRegistry>>>);

impl RegistryHandle {
    pub fn new(registry: Arc<ProviderRegistry>) -> Self {
        Self(Arc::new(tokio::sync::watch::Sender::new(registry)))
    }

    /// The registry now in force.
    pub fn current(&self) -> Arc<ProviderRegistry> {
        self.0.borrow().clone()
    }

    /// Put `next` in force. Whoever already took [`Self::current`] keeps the
    /// registry they took until they are done with it.
    pub fn replace(&self, next: Arc<ProviderRegistry>) {
        self.0.send_replace(next);
    }

    /// A receiver that wakes when [`Self::replace`] puts another registry in
    /// force.
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<Arc<ProviderRegistry>> {
        self.0.subscribe()
    }
}

impl std::fmt::Debug for RegistryHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("RegistryHandle")
            .field(&self.current())
            .finish()
    }
}

impl From<Arc<ProviderRegistry>> for RegistryHandle {
    fn from(registry: Arc<ProviderRegistry>) -> Self {
        Self::new(registry)
    }
}

/// The daemon's own read of a provider node, for a config Source's
/// `comb.<p>`: stored records only, put over source, up the directory chain.
/// It reads through the registry and the cache, and the registry owns the
/// Sources that hold it, so it is bound late and holds the registry weakly.
#[derive(Default)]
pub struct RegistryReader {
    bound: OnceLock<(Weak<ProviderRegistry>, Arc<Cache>)>,
}

impl DaemonRead for RegistryReader {
    fn ran_calling(&self, provider: &str, source: &str, dir: Option<&str>, called: CalleeMap) {
        if let Some((registry, _)) = self.bound.get()
            && let Some(registry) = registry.upgrade()
        {
            let root = dir.and_then(crate::query::address_of);
            registry.set_callees((provider, source, root.as_deref()), called);
        }
    }

    /// Runs a cold instance first and waits for it: the caller is a Source's
    /// `data` line, on a blocking thread.
    fn call(
        &self,
        provider: &str,
        source: &str,
        dir: Option<&str>,
    ) -> Result<Option<serde_json::Value>, String> {
        let Some((registry, cache)) = self.bound.get() else {
            debug_assert!(false, "a config Source read from a registry never shared");
            return Ok(None);
        };
        let Some(registry) = registry.upgrade() else {
            return Ok(None);
        };
        crate::provider::source_call::run_call(&registry, cache, provider, source, dir)
            .map(|record| record.map(|r| r.value))
    }

    /// A read like any other: the provider's cold Sources run first, once
    /// ([`crate::provider::source_call::warm_provider`]), then its stored
    /// records are read, put over source, through its `return` line. `dir` is
    /// a directory; `$HOME` addresses the global entry, as `QueryPlan`
    /// normalises it. It reads every instance up each Source's chain from
    /// `dir`, as a whole-provider read does.
    fn read(
        &self,
        provider: &str,
        dir: Option<&str>,
    ) -> Result<Option<HashMap<String, Value>>, String> {
        let Some((registry, cache)) = self.bound.get() else {
            debug_assert!(false, "a config Source read from a registry never shared");
            return Ok(None);
        };
        let Some(registry) = registry.upgrade() else {
            return Ok(None);
        };
        let Ok(plan) = crate::query::QueryPlan::build(provider, dir, &registry) else {
            return Ok(None);
        };
        if crate::query::parse_facet(&plan.provider)
            .ok()
            .flatten()
            .is_none()
        {
            crate::provider::source_call::warm_provider(
                &registry,
                cache,
                &plan.provider,
                plan.requested_path.as_deref(),
            )?;
        }
        let instances = crate::query::instances_for(
            &registry,
            &plan.provider,
            &crate::query::SourceDemand::All,
            plan.requested_path.as_deref(),
        )?;
        let merged = cache.read_merged(
            &plan.provider,
            plan.requested_path.as_deref(),
            &instances,
            &registry,
            Layers::All,
        );
        // What a `get` of the provider answers: its `return` has the last word.
        if crate::query::parse_facet(&plan.provider)
            .ok()
            .flatten()
            .is_some()
        {
            return Ok(merged.map(|m| m.fields));
        }
        let returned = crate::shape::provider::returned_over(
            cache,
            &registry,
            &plan.provider,
            plan.requested_path.as_deref(),
            merged,
        );
        Ok(returned.read.map(|m| m.fields))
    }
}

impl std::fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut names = self.provider_names();
        names.sort();
        f.debug_struct("ProviderRegistry")
            .field("providers", &names)
            .finish_non_exhaustive()
    }
}

/// A ring in the calls between Sources, as the Sources round it with the first
/// repeated at the end; the first found in name order.
fn find_ring(
    edges: &std::collections::BTreeMap<(String, String), Vec<(String, String)>>,
) -> Option<Vec<(String, String)>> {
    fn visit(
        node: &(String, String),
        edges: &std::collections::BTreeMap<(String, String), Vec<(String, String)>>,
        path: &mut Vec<(String, String)>,
        done: &mut HashSet<(String, String)>,
    ) -> Option<Vec<(String, String)>> {
        if let Some(at) = path.iter().position(|n| n == node) {
            let mut ring = path[at..].to_vec();
            ring.push(node.clone());
            return Some(ring);
        }
        if done.contains(node) {
            return None;
        }
        path.push(node.clone());
        for next in edges.get(node).into_iter().flatten() {
            if let Some(ring) = visit(next, edges, path, done) {
                return Some(ring);
            }
        }
        path.pop();
        done.insert(node.clone());
        None
    }
    let mut done = HashSet::new();
    edges
        .keys()
        .find_map(|node| visit(node, edges, &mut Vec::new(), &mut done))
}

/// A built-in Rust Source with other metadata: what its `return` line (or the
/// daemon's budget) says in place of what it declares. Everything else is the
/// Source's own: every method of the `Source` trait is forwarded, so a method
/// added to the trait with a default body must be forwarded here too, or a
/// retuned Source would quietly take the default.
struct Retuned {
    inner: Arc<dyn Source>,
    meta: SourceMetadata,
}

impl Source for Retuned {
    fn metadata(&self) -> &SourceMetadata {
        &self.meta
    }

    fn execute(&self, dir: Option<&str>) -> Result<SourceResult, String> {
        self.inner.execute(dir)
    }

    fn root(&self, dir: &str) -> Result<Option<String>, String> {
        self.inner.root(dir)
    }

    fn root_key(&self) -> String {
        self.inner.root_key()
    }

    fn has_root(&self) -> bool {
        self.inner.has_root()
    }

    fn inputs(
        &self,
        declared: &crate::provider::InvalidationStrategy,
        root: Option<&str>,
    ) -> Vec<std::path::PathBuf> {
        self.inner.inputs(declared, root)
    }
}

/// How long a failed `root` line's error is answered again without re-running
/// the line. Timed on tokio's clock, so a test drives it with
/// `tokio::time::advance`.
const ROOT_RETRY_TTL: std::time::Duration = std::time::Duration::from_secs(5);

/// How long the nothing a nearest Source's `root` found is kept before a new
/// one sweeps it: no instance's eviction clears it, so without a sweep a
/// long-lived daemon would hold one per directory ever asked about. Its
/// stamps, not this, decide when it is rendered again.
const UNCLAIMED_TTL: std::time::Duration = std::time::Duration::from_secs(600);

/// A rendered `root`, kept for one directory.
struct KeptRoot {
    found: Found,
    /// The directories the answer depends on, from the one asked about up to
    /// the root found (up to `$HOME`, or `/` outside it, when it found none),
    /// stamped before the line rendered. Creating or removing a marker in one
    /// moves its mtime, and the root is rendered again.
    walked: Option<crate::cache::Validity>,
    /// When the nothing a nearest Source's root found was kept, for the sweep
    /// ([`UNCLAIMED_TTL`]); `None` for an answer an eviction clears.
    unclaimed_since: Option<tokio::time::Instant>,
}

/// The directories a `root` line's answer for `dir` can depend on, nearest
/// first: `dir` and each above it, up to `$HOME` (or `/` outside it).
fn walk_up(dir: &str) -> Vec<std::path::PathBuf> {
    let home = crate::query::home_dir();
    let mut out = Vec::new();
    let mut at = Some(std::path::Path::new(dir));
    while let Some(here) = at {
        out.push(here.to_path_buf());
        if home.as_deref() == Some(here) {
            break;
        }
        at = here.parent();
    }
    out
}

/// How many of `walked` (from [`walk_up`]) the answer `found` depends on: up
/// to the root it found when that is one of them, else all of them.
fn walked_to(walked: &[std::path::PathBuf], found: &Found) -> usize {
    let root = match found {
        Found::At(root) => Some(std::path::PathBuf::from(root)),
        Found::Home => crate::query::home_dir(),
        Found::Nothing => None,
    };
    root.and_then(|root| walked.iter().position(|dir| *dir == root))
        .map_or(walked.len(), |at| at + 1)
}

pub struct ProviderRegistry {
    /// (provider_name, source_name) → Arc-wrapped Source trait object.
    sources: HashMap<(String, String), Arc<dyn Source>>,
    /// Provider metadata snapshot, indexed by provider name.
    providers: HashMap<String, ProviderMetadata>,
    /// (provider_name, declared dotted field) → the Sources declaring it, in
    /// declaration order (more than one only under a `result` that merges them
    /// itself), sentinels excluded. Built once at registration.
    declared_sources: HashMap<(String, String), Vec<String>>,
    /// provider_name → Sources declaring a sentinel field (`<tool>`), in
    /// declaration order. They answer for the provider node.
    sentinel_sources: HashMap<String, Vec<String>>,
    /// Static namespace names registered via `comb put`. Orthogonal to the source model.
    static_names: Arc<DashMap<String, ()>>,
    /// The `root` of a config Source, by (provider, source, directory): the
    /// line is rendered once per directory (it may run a command) and kept
    /// while the directories on its walk are as they were, until the instance
    /// it names is evicted.
    roots: DashMap<(String, String, String), KeptRoot>,
    /// A `root` line that failed, by (provider, source, directory), with when
    /// and what it said: answered again for [`ROOT_RETRY_TTL`] rather than
    /// re-run, so a slow failing line costs its timeout once, not per request.
    root_errors: DashMap<(String, String, String), (tokio::time::Instant, String)>,
    /// Consecutive failures and failback backoff, per instance. Every run
    /// records here, whoever started it, so an instance in backoff is skipped
    /// by all of them.
    pub(crate) failures: crate::scheduler::SourceFailureMap,
    /// What every config Source reads `comb.<p>` through.
    reader: Arc<RegistryReader>,
    /// Every provider's `template`, `result` and `return` lines, compiled.
    lines: Arc<ProviderLines>,
    /// The config Sources registered, as (provider, source).
    config_sources: HashSet<(String, String)>,
    /// The config Sources with a `root` line, as (provider, source).
    rendered_roots: HashSet<(String, String)>,
    /// The Sources each config Source's `data` line calls, as (provider,
    /// source, called): checked against the Sources that exist once all are
    /// registered.
    data_calls: Vec<(String, String, Called)>,
    /// Every run of every instance, whoever started it: shared by every
    /// registry of a daemon, so a run under way at a reload is the one a
    /// caller on the new registry finds.
    runs: Arc<crate::run::Runs>,
    /// The instances each instance's last run called, by (provider, source,
    /// root): demanding an instance demands what it calls, so a Source called
    /// only by one that is decaying decays with it.
    callees: DashMap<(String, String, Option<String>), CalleeMap>,
    /// The plugin libraries this registry's Sources call, when any config
    /// Source was built. A reload reuses the set, so a library loaded once
    /// stays loaded for the daemon's life and is never closed under a running
    /// call.
    plugins: Option<Arc<PluginSet>>,
    /// What each config Source is made of, apart from when it runs: its `root`,
    /// `data` and `result` lines as resolved, `_` prototypes included. A reload
    /// compares them to tell a Source whose body changed from one that was only
    /// rescheduled.
    definitions: HashMap<(String, String), String>,
    /// Which Sources a reload has retired since this registry was built; shared
    /// by every registry of a daemon, so a run holding an old one is told too.
    retirements: Arc<Retirements>,
    /// The reload epoch this registry was put in force at (see [`Retirements`]).
    born: std::sync::atomic::AtomicU64,
}

/// The Sources a reload removed or changed, by the reload epoch that did it.
/// A run that began under a registry older than that epoch holds a result
/// computed from a body that is no longer in force. The write of a result takes
/// the read side ([`ProviderRegistry::unless_retired`]) and a reload the write
/// side, so a write is either finished before the reload evicts or skipped.
#[derive(Default)]
struct Retirements {
    gate: std::sync::RwLock<RetiredAt>,
}

#[derive(Default)]
struct RetiredAt {
    epoch: u64,
    at: HashMap<(String, String), u64>,
}

/// The instances a run called: (provider, source) to the roots reached.
pub type CalleeMap = HashMap<(String, String), Vec<Option<String>>>;

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            sources: HashMap::new(),
            providers: HashMap::new(),
            declared_sources: HashMap::new(),
            sentinel_sources: HashMap::new(),
            static_names: Arc::new(DashMap::new()),
            roots: DashMap::new(),
            root_errors: DashMap::new(),
            failures: Arc::default(),
            reader: Arc::new(RegistryReader::default()),
            lines: Arc::new(
                ProviderLines::new(&toml::Table::new())
                    .expect("a set of lines with no tables loads"),
            ),
            config_sources: HashSet::new(),
            rendered_roots: HashSet::new(),
            data_calls: Vec::new(),
            runs: Arc::default(),
            callees: DashMap::new(),
            plugins: None,
            definitions: HashMap::new(),
            retirements: Arc::default(),
            born: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// The providers' own lines: what a read of a provider's records, and the
    /// facets that ride beside it, are made of.
    pub fn lines(&self) -> &ProviderLines {
        &self.lines
    }

    /// Every run of every instance ([`crate::run::Runs`]): where a run is
    /// started, whoever starts it.
    pub fn runs(&self) -> &Arc<crate::run::Runs> {
        &self.runs
    }

    /// The reload epoch this registry was put in force at: a later registry
    /// has a larger one.
    pub fn epoch(&self) -> u64 {
        self.born.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Replace what the instance `caller` (provider, source, root) is known
    /// to call with what its last run called: a callee it no longer calls is
    /// no longer demanded with it. Nothing called removes the entry.
    pub fn set_callees(&self, caller: (&str, &str, Option<&str>), called: CalleeMap) {
        let key = (
            caller.0.to_string(),
            caller.1.to_string(),
            caller.2.map(str::to_string),
        );
        if called.is_empty() {
            self.callees.remove(&key);
        } else {
            self.callees.insert(key, called);
        }
    }

    /// Forget what the instance at `root` called: the scheduler calls this
    /// beside [`Self::forget_roots`] when the instance is evicted.
    pub fn forget_callees(&self, provider: &str, source: &str, root: Option<&str>) {
        self.callees
            .remove(&(provider.into(), source.into(), root.map(str::to_string)));
    }

    /// Forget what every instance of a Source was known to call.
    pub fn forget_callees_of(&self, provider: &str, source: &str) {
        self.callees
            .retain(|(p, s, _), _| !(p == provider && s == source));
    }

    /// The Sources `next` removes, disables, enables or changes the body of,
    /// and the Sources that call them, transitively (by what their last runs
    /// called): what they stored was computed from an answer that goes. A
    /// provider switched on again is as changed as one switched off: a call to
    /// it was a miss and now is not.
    pub fn retired_by(&self, next: &Self) -> HashSet<(String, String)> {
        let mut out: HashSet<(String, String)> = self
            .sources
            .keys()
            .filter(|(p, s)| {
                self.is_disabled(p) != next.is_disabled(p)
                    || next.source(p, s).is_none()
                    || self.definition(p, s) != next.definition(p, s)
            })
            .cloned()
            .collect();
        // By Source, not instance: a call that reached no instance (a disabled
        // provider, a nearest Source with no root) is a dependency too.
        let mut callers: HashMap<(String, String), Vec<(String, String)>> = HashMap::new();
        for entry in self.callees.iter() {
            let (provider, source, _) = entry.key();
            for callee in entry.value().keys() {
                callers
                    .entry(callee.clone())
                    .or_default()
                    .push((provider.clone(), source.clone()));
            }
        }
        let mut pending: Vec<_> = out.iter().cloned().collect();
        while let Some(callee) = pending.pop() {
            for caller in callers.get(&callee).into_iter().flatten() {
                if out.insert(caller.clone()) {
                    pending.push(caller.clone());
                }
            }
        }
        out
    }

    /// Retire `sources` as `next` takes over from this registry: from here a
    /// run that began under this registry (or any older) skips its write for
    /// them. Waits for the writes in progress.
    pub fn retire(&self, next: &Self, sources: &HashSet<(String, String)>) {
        let mut gate = self
            .retirements
            .gate
            .write()
            .unwrap_or_else(|e| e.into_inner());
        gate.epoch += 1;
        let epoch = gate.epoch;
        for source in sources {
            gate.at.insert(source.clone(), epoch);
        }
        next.born.store(epoch, std::sync::atomic::Ordering::SeqCst);
    }

    /// Run `write` (a result's write into the cache) unless a reload has retired
    /// the Source since this registry was put in force; `None` when skipped.
    /// Holds the read side of the reload's gate while it runs.
    pub fn unless_retired<R>(
        &self,
        provider: &str,
        source: &str,
        write: impl FnOnce() -> R,
    ) -> Option<R> {
        let gate = self
            .retirements
            .gate
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let born = self.born.load(std::sync::atomic::Ordering::SeqCst);
        // Nothing retired (the common case): no key to build.
        if !gate.at.is_empty()
            && gate
                .at
                .get(&(provider.to_string(), source.to_string()))
                .is_some_and(|&epoch| epoch > born)
        {
            return None;
        }
        Some(write())
    }

    /// The lifecycle keys of the instances called, by the keys of the
    /// instances that call them: a callee is held while any of its callers is.
    pub fn callers_of(
        &self,
    ) -> HashMap<crate::scheduler::lifecycle::Key, Vec<crate::scheduler::lifecycle::Key>> {
        use crate::provider::source_call::instance_key;
        let mut out: HashMap<_, Vec<_>> = HashMap::new();
        for entry in self.callees.iter() {
            let (provider, source, root) = entry.key();
            let caller = instance_key(provider, source, root.as_deref());
            for ((cp, cs), roots) in entry.value() {
                for callee_root in roots {
                    out.entry(instance_key(cp, cs, callee_root.as_deref()))
                        .or_default()
                        .push(caller.clone());
                }
            }
        }
        out
    }

    /// `instances` of `provider`, and the instances they called, and those
    /// theirs: what a demand for them is a demand for.
    pub fn with_callees(
        &self,
        provider: &str,
        instances: &[crate::query::Instance],
    ) -> Vec<(String, crate::query::Instance)> {
        if self.callees.is_empty() {
            return instances
                .iter()
                .map(|i| (provider.to_string(), i.clone()))
                .collect();
        }
        let mut out: Vec<(String, crate::query::Instance)> = Vec::new();
        let mut pending: Vec<(String, crate::query::Instance)> = instances
            .iter()
            .map(|i| (provider.to_string(), i.clone()))
            .collect();
        while let Some((p, instance)) = pending.pop() {
            if out.iter().any(|(op, oi)| *op == p && *oi == instance) {
                continue;
            }
            let key = (p.clone(), instance.source.clone(), instance.root.clone());
            if let Some(called) = self.callees.get(&key) {
                for ((cp, cs), roots) in called.iter() {
                    for root in roots {
                        pending
                            .push((cp.clone(), crate::query::Instance::new(cs, root.as_deref())));
                    }
                }
            }
            out.push((p, instance));
        }
        out.reverse();
        out
    }

    /// Share the registry the way a running daemon does: behind an `Arc`,
    /// with its config Sources able to read `comb.<p>` from `cache`. The only
    /// way to a registry whose config Sources can read; a registry never
    /// shared this way reads nothing.
    pub fn share(self, cache: &Arc<Cache>) -> Arc<Self> {
        let shared = Arc::new(self);
        shared.bind(cache);
        shared
    }

    /// Let config Sources read `comb.<p>` from `cache`: [`Self::share`]'s half.
    fn bind(self: &Arc<Self>, cache: &Arc<Cache>) {
        let bound = self.reader.bound.set((Arc::downgrade(self), cache.clone()));
        assert!(bound.is_ok(), "a registry is bound once");
    }

    /// Whether `provider`'s Source `source` finds its root by rendering a
    /// `root` line, which may run a command, fetch a URL or call a plugin. Its
    /// answers are kept per directory, and a first render must not hold an
    /// async worker. A built-in finds its root by looking at the filesystem
    /// every time, so a marker created after the first request is seen.
    pub fn renders_root(&self, provider: &str, source: &str) -> bool {
        self.rendered_roots
            .contains(&(provider.to_string(), source.to_string()))
    }

    /// The kept root of the Source for `dir`, if it has been rendered and the
    /// directories on its walk are as they were: never renders. `Some(found)`
    /// is the answer ([`Self::instance_root`]'s). One `stat` per directory
    /// from `dir` up to the root found (up to `$HOME` when it found none): a
    /// marker created or removed in one moves its mtime, and the root is
    /// dropped, to be rendered again.
    pub fn kept_root(&self, provider: &str, source: &str, dir: &str) -> Option<Found> {
        let key = (provider.to_string(), source.to_string(), dir.to_string());
        let (found, walked) = {
            let kept = self.roots.get(&key)?;
            (kept.found.clone(), kept.walked.clone())
        };
        // Stamped with no lock held: a stat is a syscall.
        if walked.is_some_and(|w| w.check() == libbeachcomber::validity::Check::Invalid) {
            self.roots.remove(&key);
            return None;
        }
        Some(found)
    }

    /// A walk for `provider`'s Source `source` from `dir` (the plan's
    /// `requested_path`), shaped by the Source's `nearest` setting. A Source
    /// that is not known, or has no root, cascades: its one instance is `~`.
    pub fn instance_walk(
        &self,
        provider: &str,
        source: &str,
        dir: Option<&str>,
    ) -> crate::query::InstanceWalk {
        let nearest = self
            .source(provider, source)
            .is_some_and(|s| s.has_root() && s.metadata().nearest);
        crate::query::InstanceWalk::new(dir, nearest)
    }

    /// What `provider`'s Source `source` finds as its root for `dir`:
    /// [`Found::At`] a directory, [`Found::Home`] when that is `$HOME` (the
    /// instance at `~`, see [`crate::query::normalise_path`]), or
    /// [`Found::Nothing`]. What nothing means is the walk's
    /// ([`crate::query::InstanceWalk`]).
    ///
    /// A config Source renders its `root` line once per directory and the
    /// answer is kept while the directories on its walk are as they were
    /// ([`Self::kept_root`]), until the instance it names is evicted
    /// ([`Self::forget_roots`]); an error is kept for [`ROOT_RETRY_TTL`] only.
    /// May run a command: callers on an async worker go through
    /// [`Self::renders_root`] and [`Self::kept_root`] first.
    pub fn instance_root(&self, provider: &str, source: &str, dir: &str) -> Result<Found, String> {
        if let Some(kept) = self.kept_root(provider, source, dir) {
            return Ok(kept);
        }
        let src = self
            .source(provider, source)
            .ok_or_else(|| format!("{provider}.{source}: no such source"))?;
        let error_key = (provider.to_string(), source.to_string(), dir.to_string());
        if let Some(failed) = self.root_errors.get(&error_key) {
            if failed.0.elapsed() < ROOT_RETRY_TTL {
                return Err(failed.1.clone());
            }
            drop(failed);
            self.root_errors.remove(&error_key);
        }
        // Two requests that both miss the kept root for a directory at once
        // each render the line, and the later insert replaces the earlier: a
        // duplicate first render, not a wrong answer, since both ask the same
        // question of the same directory.
        //
        // The walk is stamped before the line renders: a marker created while
        // it renders is a change the next read sees.
        let walked = self
            .renders_root(provider, source)
            .then(|| crate::cache::Stamped::take(walk_up(dir)));
        let found = src.root(dir);
        if let Err(msg) = &found
            && self.renders_root(provider, source)
        {
            // Lapsed answers are never read again: sweep them as new ones come
            // in, so a long-lived daemon does not hold one per directory ever
            // asked about.
            self.root_errors
                .retain(|_, (at, _)| at.elapsed() < ROOT_RETRY_TTL);
            self.root_errors
                .insert(error_key, (tokio::time::Instant::now(), msg.clone()));
        }
        let root = match found? {
            // A built-in finds its root by walking up from `dir`, which is
            // canonical: an ancestor of it is too, and only `~` is special.
            Some(root) if !self.renders_root(provider, source) => {
                if crate::query::is_home(&root) {
                    Found::Home
                } else {
                    Found::At(root)
                }
            }
            // A rendered root is whatever the line says: canonicalise it.
            Some(root) if root == dir && !crate::query::is_home(&root) => Found::At(root),
            Some(root) => match crate::query::address_of(&root) {
                Some(root) => Found::At(root),
                None => Found::Home,
            },
            None => Found::Nothing,
        };
        if let Some(mut walked) = walked {
            let now = tokio::time::Instant::now();
            let unclaimed = root == Found::Nothing && src.has_root() && src.metadata().nearest;
            // An old unclaimed answer goes as a new one comes in, so a
            // long-lived daemon does not hold one per directory ever asked
            // about.
            if unclaimed {
                self.roots.retain(|_, kept| {
                    kept.unclaimed_since
                        .is_none_or(|at| now.duration_since(at) < UNCLAIMED_TTL)
                });
            }
            let depends = walked_to(&walk_up(dir), &root);
            walked.truncate(depends);
            self.roots.insert(
                (provider.to_string(), source.to_string(), dir.to_string()),
                KeptRoot {
                    found: root.clone(),
                    walked: crate::cache::Validity::of(walked),
                    unclaimed_since: unclaimed.then_some(now),
                },
            );
        }
        Ok(root)
    }

    /// Drop the kept roots of `provider`'s Source `source` that resolved to
    /// the instance at `root` (`None` is the instance at `~`): the scheduler
    /// calls this when that instance is evicted, so a root is looked up again
    /// once the instance it named has gone cold. A change the walk's stamps
    /// do not see (what a command in the line reads) is not seen until then.
    pub fn forget_roots(&self, provider: &str, source: &str, root: Option<&str>) {
        self.roots.retain(|(p, s, _), kept| {
            // Home and Nothing are both answered by the instance at `~`: for
            // a Source that cascades, nothing found is served there.
            let instance = match &kept.found {
                Found::At(at) => Some(at.as_str()),
                Found::Home | Found::Nothing => None,
            };
            !(p == provider && s == source && instance == root)
        });
    }

    /// Register a provider. Validates metadata, builds the field→source and sentinel maps,
    /// and inserts all sources. Returns `Err` if the provider name is already registered,
    /// metadata is invalid, or sources() doesn't match metadata.
    pub fn register(&mut self, provider: Box<dyn Provider>) -> Result<(), String> {
        let meta = provider.metadata();
        meta.validate(self.merges_by_default(&meta.name))?;
        if self.providers.contains_key(&meta.name) {
            return Err(format!("provider '{}' already registered", meta.name));
        }
        let sources = provider.sources();
        if sources.len() != meta.sources.len() {
            return Err(format!(
                "provider '{}' metadata declares {} sources but sources() returned {}",
                meta.name,
                meta.sources.len(),
                sources.len()
            ));
        }
        // Validate that source trait objects line up with metadata by name.
        let names: std::collections::HashSet<String> =
            sources.iter().map(|s| s.metadata().name.clone()).collect();
        for sm in &meta.sources {
            if !names.contains(&sm.name) {
                return Err(format!(
                    "provider '{}' metadata declares source '{}' but sources() did not include it",
                    meta.name, sm.name
                ));
            }
        }
        for sm in &meta.sources {
            self.index_fields(&meta.name, sm);
        }
        // Insert sources.
        for s in sources {
            let sn = s.metadata().name.clone();
            self.sources.insert((meta.name.clone(), sn), Arc::from(s));
        }
        self.providers.insert(meta.name.clone(), meta);
        Ok(())
    }

    /// Whether a read of `provider` is its Sources' records as stored (the
    /// default `result`, an identity `return`): then a field is the one Source
    /// that declares it. Under any other line the line decides what exists, a
    /// field read demands every Source, and nothing short of the whole chain
    /// answers it.
    pub fn reads_by_default(&self, provider: &str) -> bool {
        self.lines.chains_for(provider).reads_by_default()
    }

    /// Whether `provider` is switched off (`enabled = false`): none of its
    /// Sources is read, run, polled or watched, and a call to one is a miss.
    pub fn is_disabled(&self, provider: &str) -> bool {
        self.lines.is_disabled(provider)
    }

    /// Whether `provider`'s `result` is the default merge of its Sources, under
    /// which two of them may not declare one field.
    fn merges_by_default(&self, provider: &str) -> bool {
        self.lines.chains_for(provider).result_is_default()
    }

    /// Fill the declared-field map and the sentinel list from one Source.
    fn index_fields(&mut self, provider: &str, sm: &SourceMetadata) {
        for f in &sm.fields {
            if is_sentinel(&f.name) {
                let list = self
                    .sentinel_sources
                    .entry(provider.to_string())
                    .or_default();
                if !list.contains(&sm.name) {
                    list.push(sm.name.clone());
                }
            } else {
                let owners = self
                    .declared_sources
                    .entry((provider.to_string(), f.name.clone()))
                    .or_default();
                if !owners.contains(&sm.name) {
                    owners.push(sm.name.clone());
                }
            }
        }
    }

    /// Add config Sources to `provider`, creating it when no code registered
    /// it: a new Source of a built-in provider joins that provider.
    pub fn register_config_sources(
        &mut self,
        provider: &str,
        sources: Vec<ConfigSource>,
    ) -> Result<(), String> {
        let mut meta = self
            .providers
            .get(provider)
            .cloned()
            .unwrap_or_else(|| ProviderMetadata {
                name: provider.to_string(),
                sources: Vec::new(),
            });
        let first_new = meta.sources.len();
        meta.sources
            .extend(sources.iter().map(|s| s.metadata().clone()));
        meta.validate(self.merges_by_default(provider))?;
        for sm in &meta.sources[first_new..] {
            self.index_fields(provider, sm);
        }
        for source in sources {
            let key = (provider.to_string(), source.metadata().name.clone());
            self.data_calls.extend(
                source
                    .calls()
                    .iter()
                    .map(|c| (key.0.clone(), key.1.clone(), c.clone())),
            );
            if source.has_root() {
                self.rendered_roots.insert(key.clone());
            }
            self.config_sources.insert(key.clone());
            self.definitions.insert(key.clone(), source.definition());
            self.sources.insert(key, Arc::new(source));
        }
        self.providers.insert(provider.to_string(), meta);
        Ok(())
    }

    /// How many config Sources are registered: the Sources the files declare,
    /// counting those whose lines come from a `_` prototype, and not the
    /// built-ins a `return` retunes.
    pub fn config_source_count(&self) -> usize {
        self.config_sources.len()
    }

    /// Every registered Source's metadata, by provider: what a config table
    /// may not redefine.
    pub fn registered_sources(&self) -> HashMap<String, Vec<SourceMetadata>> {
        self.providers
            .iter()
            .map(|(name, meta)| (name.clone(), meta.sources.clone()))
            .collect()
    }

    /// The config Sources `config` declares, built and linted against the
    /// Sources already registered. Every error is a load error.
    pub fn config_sources(
        &self,
        config: &Config,
        env: &HashMap<String, String>,
    ) -> (ConfigSources, Vec<String>) {
        let Some(providers) = config.daemon().providers.as_table() else {
            return (ConfigSources::default(), Vec::new());
        };
        let mut errors = Vec::new();
        // A bad `[daemon.plugin] path` is one error among the config's: the
        // Sources are still checked, with nothing on the search path.
        let search = config
            .plugin_path(env.get("HOME").map(String::as_str))
            .unwrap_or_else(|e| {
                errors.push(e);
                Vec::new()
            });
        let plugins = Arc::new(PluginSet::new(search, Arc::new(LibloadingLoader)));
        let env = Arc::new(env.clone());

        // A `_` table's lines apply to Sources below it; they are compiled
        // here as a Source's own are, so a broken prototype does not load.
        for proto in crate::shape::prototype_lines(providers) {
            errors.extend(lint_lines(
                &proto.path,
                proto.root.as_deref(),
                proto.data.as_deref(),
                proto.result.as_ref(),
                &env,
                &plugins,
            ));
        }
        // What builds is checked further whatever else does not: one broken
        // table hides no other.
        let (built, shape_errors) = crate::shape::build(
            providers,
            &self.registered_sources(),
            &engine_defaults(config),
        );
        errors.extend(shape_errors);
        let (decls, retunes) = (built.decls, built.retunes);
        let mut out = Vec::new();
        for decl in decls {
            let mut built = Vec::new();
            for spec in decl.sources {
                match ConfigSource::new(spec, env.clone(), plugins.clone(), self.reader.clone()) {
                    Ok(source) => built.push(source),
                    Err(e) => errors.extend(e),
                }
            }
            // A provider none of whose Sources built has their errors; it is
            // not registered a second time with nothing to say for itself.
            if !built.is_empty() {
                out.push((decl.name, built));
            }
        }
        (
            ConfigSources {
                providers: out,
                retunes,
                plugins: Some(plugins),
            },
            errors,
        )
    }

    /// Replace a registered built-in Source's metadata with `edit` of it. The
    /// Source runs as before: only what the scheduler reads about it changes.
    fn retune(&mut self, provider: &str, source: &str, edit: impl FnOnce(&mut SourceMetadata)) {
        let key = (provider.to_string(), source.to_string());
        let Some(inner) = self.sources.get(&key).cloned() else {
            return;
        };
        let mut meta = inner.metadata().clone();
        edit(&mut meta);
        if let Some(listed) = self
            .providers
            .get_mut(provider)
            .and_then(|p| p.sources.iter_mut().find(|s| s.name == source))
        {
            *listed = meta.clone();
        }
        self.sources.insert(key, Arc::new(Retuned { inner, meta }));
    }

    /// Register a provider only configuration names: it has zero
    /// Sources, its templates and cache settings being data. This is the one
    /// way to a provider with none; `register` refuses a provider that
    /// declares no Sources. Returns false if the name is already registered.
    pub fn register_config_named(&mut self, name: &str) -> bool {
        if self.providers.contains_key(name) {
            return false;
        }
        self.providers.insert(
            name.to_string(),
            ProviderMetadata {
                name: name.to_string(),
                sources: Vec::new(),
            },
        );
        true
    }

    // ── Lookup methods ───────────────────────────────────────────────────────

    /// Return the provider metadata for a given provider name.
    pub fn provider_metadata(&self, provider: &str) -> Option<&ProviderMetadata> {
        self.providers.get(provider)
    }

    /// Return the Arc-wrapped Source for a (provider, source) pair.
    pub fn source(&self, provider: &str, source: &str) -> Option<Arc<dyn Source>> {
        self.sources
            .get(&(provider.to_string(), source.to_string()))
            .cloned()
    }

    /// The Sources that answer `field` (a canonical address below the
    /// provider): the Source declared at the nearest ancestor node of the key —
    /// `field` itself, then each shorter prefix (`foo.bar.baz` tries
    /// `foo.bar.baz`, `foo.bar`, `foo`) — else every Source that declares a
    /// sentinel field (they answer for the provider node), in declaration
    /// order, else none.
    pub fn sources_for_key(&self, provider: &str, field: &str) -> Vec<&str> {
        let mut node = field.to_string();
        while !node.is_empty() {
            if let Some(owners) = self
                .declared_sources
                .get(&(provider.to_string(), node.clone()))
            {
                return owners.iter().map(String::as_str).collect();
            }
            node = libbeachcomber::key::parent(&node).unwrap_or_default();
        }
        self.sentinel_sources
            .get(provider)
            .map(|v| v.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }

    /// The metadata of one Source, as registered (a retuned built-in's own).
    pub fn source_metadata(&self, provider: &str, source: &str) -> Option<&SourceMetadata> {
        self.provider_sources(provider)?
            .iter()
            .find(|sm| sm.name == source)
    }

    /// Return the slice of SourceMetadata for all sources declared by a provider.
    pub fn provider_sources(&self, provider: &str) -> Option<&[SourceMetadata]> {
        self.providers.get(provider).map(|m| m.sources.as_slice())
    }

    /// Return the list of registered provider names (excluding static ones).
    pub fn provider_names(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }

    // ── Static namespace support ─────────────────────────────────────────────

    /// Returns true if the provider name belongs to an already-registered
    /// (builtin or config) provider.
    pub fn is_registered(&self, name: &str) -> bool {
        self.providers.contains_key(name)
    }

    /// Returns true if the name was registered as a static namespace.
    pub fn is_static_namespace(&self, name: &str) -> bool {
        !self.is_registered(name) && self.static_names.contains_key(name)
    }

    /// Register a static namespace: one no code or config registered, where
    /// every record carries a static source (`put`, or later `config:<file>`)
    /// and `comb status` shows its rows as kind `static`. Returns false if an
    /// already-registered provider exists with this name.
    pub fn register_static(&self, name: &str) -> bool {
        if self.is_registered(name) {
            return false;
        }
        self.static_names.insert(name.to_string(), ());
        true
    }

    /// Return all provider names, including static namespace names.
    pub fn list(&self) -> Vec<String> {
        let mut names: Vec<String> = self.providers.keys().cloned().collect();
        for entry in self.static_names.iter() {
            let name = entry.key().clone();
            if !names.contains(&name) {
                names.push(name);
            }
        }
        names
    }

    // ── Construction helpers ─────────────────────────────────────────────────

    /// The registry a daemon starting on the shipped defaults alone loads:
    /// every `defaults/` file composed as the daemon composes them
    /// ([`crate::daemon::defaults::shipped_layers`]), its Sources' lines
    /// seeing `vars`. What a test or a bench builds its registry from: every
    /// new one does, so that a provider moving from Rust to a shipped file
    /// changes no test. Panics if the shipped files do not load.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn shipped(vars: &HashMap<String, String>) -> Self {
        let config = Config::from_layers(crate::daemon::defaults::shipped_layers());
        let load = Self::load(&config, vars);
        assert!(
            load.errors.is_empty(),
            "the shipped defaults do not load: {:?}",
            load.errors
        );
        load.registry
    }

    /// Build a registry with all built-in providers registered: what
    /// [`Self::load`] starts from. A test or a bench builds from
    /// [`Self::shipped`] instead, never from this.
    /// Panics on registration failure (programmer error, not runtime).
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        registry
            .register(Box::new(crate::provider::asdf::AsdfProvider))
            .expect("asdf");
        registry
            .register(Box::new(crate::provider::aws::AwsProvider))
            .expect("aws_profiles");
        registry
            .register(Box::new(crate::provider::direnv::DirenvProvider))
            .expect("direnv");
        registry
            .register(Box::new(crate::provider::gcloud::GcloudProvider))
            .expect("gcloud_configs");
        registry
            .register(Box::new(crate::provider::git::GitProvider))
            .expect("git");
        registry
            .register(Box::new(crate::provider::python::PythonProvider))
            .expect("python");
        registry
            .register(Box::new(crate::provider::sudo::SudoProvider))
            .expect("sudo");
        registry
            .register(Box::new(crate::provider::terraform::TerraformProvider))
            .expect("terraform");
        registry
    }

    /// [`Self::load`]: the registry, or every error the config has.
    pub fn with_config(
        config: &Config,
        vars: &HashMap<String, String>,
    ) -> Result<Self, Vec<String>> {
        let load = Self::load(config, vars);
        if load.errors.is_empty() {
            Ok(load.registry)
        } else {
            Err(load.errors)
        }
    }

    /// The one loader: startup, the config watch's gate and `comb check
    /// config` all build the provider set here, so they agree on what loads.
    /// Built-ins, the config Sources
    /// (`[daemon.providers.<p>.sources.<s>]`, whose lines see `vars`, the
    /// daemon's own variables, and call `plugin()` on `[daemon.plugin] path`),
    /// the built-in Sources a `return` retunes, and the providers only the
    /// config names; then every check a config has: the per-file ones and the
    /// Sources against the set built.
    ///
    /// Panics on built-in registration failure (programmer error).
    pub fn load(config: &Config, vars: &HashMap<String, String>) -> ConfigLoad {
        // Start from the full built-in set, then layer on config-defined providers.
        let mut registry = Self::with_defaults();

        // ── The providers' own lines ──────────────────────────────────────────
        // First: what a provider's `result` says decides what its Sources may
        // declare.
        let mut errors = Vec::new();
        let no_providers = toml::Table::new();
        let providers = config
            .daemon()
            .providers
            .as_table()
            .unwrap_or(&no_providers);
        match ProviderLines::new(providers) {
            Ok(lines) => registry.lines = Arc::new(lines),
            Err(e) => errors.extend(e),
        }

        // ── Config Sources, and the built-ins a return retunes ────────────────
        let builtins: Vec<(String, String)> = registry.sources.keys().cloned().collect();
        let (
            ConfigSources {
                providers: built,
                retunes,
                plugins,
            },
            source_errors,
        ) = registry.config_sources(config, vars);
        errors.extend(source_errors);
        registry.plugins = plugins;
        for (name, sources) in built {
            if let Err(e) = registry.register_config_sources(&name, sources) {
                errors.push(e);
            }
        }
        errors.extend(registry.retune_builtins(config, &builtins, &retunes));

        // ── Providers only the config names ───────────────────────────────────
        // A provider exists if either root names it. One named only in
        // `[client.providers]` or `[daemon.providers]` has no Source behind
        // it: it is registered with zero. `_`, the prototype's name, is never
        // one (`Config::named_providers`).
        // A table that only switches a provider off, for a name that no
        // built-in, Source or client table gives a provider, is a typo.
        for (name, table) in providers {
            if table
                .as_table()
                .is_some_and(|t| t.len() == 1 && t.contains_key("enabled"))
                && name != "_"
                && !registry.is_registered(name)
                && !config.client_names(name)
            {
                errors.push(format!(
                    "[daemon.providers.{name}] enabled: no provider is named '{name}' (no \
                     built-in, Source or template gives it one)"
                ));
            }
        }
        for name in config.named_providers() {
            registry.register_config_named(&name);
        }

        // A Source that did not build is not registered; a call to it is not
        // a second error for the same fault (`check_calls` knows the tables).
        errors.extend(registry.check_calls(providers));
        errors.extend(config.errors());
        // What the reseed could not plan (a `template` line that renders no
        // table, a client template that does not compile): the same refusal a
        // reload makes, so startup, `comb check config` and the config watch
        // all see it. What the load and the per-file rules already said is not
        // said twice.
        let (seed_errors, mut warnings) = crate::daemon::seed::plan_findings(config, &registry);
        for e in seed_errors {
            if !errors.contains(&e) {
                errors.push(e);
            }
        }
        // `sys` is the compiled-in plugin's name: a library of that name on
        // the path is never loaded, which its author should hear of.
        if let Some(shadowed) = registry.plugins.as_ref().and_then(|p| p.shadowed_sys()) {
            warnings.push(config.locate(format!(
                "[daemon.plugin] path: {} is shadowed by the built-in sys plugin, and never \
                 loaded",
                shadowed.display()
            )));
        }
        // Every error in one form: `config(<file>): [table] …`, the file the
        // one that set the table it is about.
        let mut located: Vec<String> = Vec::with_capacity(errors.len());
        for e in errors.into_iter().map(|e| config.locate(e)) {
            if !located.contains(&e) {
                located.push(e);
            }
        }
        ConfigLoad {
            registry,
            errors: located,
            warnings,
        }
    }

    /// Make `self`, freshly loaded from the configuration that replaces
    /// `previous`, the registry to put in force: bound to `cache` and carrying
    /// over what a reload must not lose.
    ///
    /// - The plugin libraries `previous` has loaded, which stay loaded for the
    ///   daemon's life and are never closed under a call that is running.
    /// - The failure state, which belongs to the instances: a Source that
    ///   survives keeps its count and backoff. The scheduler resets the ones
    ///   whose schedule changed and drops those of Sources that are gone.
    /// - The static namespaces `put` created, the same set for every registry
    ///   so a `put` racing the reload is not lost, and the namespace of a
    ///   provider that has gone but still holds puts.
    /// - What each surviving Source's instances were last known to call.
    ///
    /// The kept roots are not carried over: roots may have changed, and this
    /// registry renders them afresh. Its `comb.<p>` reader is its own, bound
    /// to it here.
    pub fn adopt(mut self, previous: &Self, cache: &Arc<Cache>) -> Arc<Self> {
        if let (Some(mine), Some(theirs)) = (&self.plugins, &previous.plugins) {
            mine.adopt_loaded(theirs);
        }
        self.failures = Arc::clone(&previous.failures);
        // A run under way at a reload is the one a caller on this registry
        // finds, as one on the last would.
        self.runs = Arc::clone(&previous.runs);
        self.static_names = Arc::clone(&previous.static_names);
        self.retirements = Arc::clone(&previous.retirements);
        for entry in previous.callees.iter() {
            let (provider, source, _) = entry.key();
            if self
                .sources
                .contains_key(&(provider.clone(), source.clone()))
            {
                self.callees
                    .insert(entry.key().clone(), entry.value().clone());
            }
        }
        for name in cache.names_with_puts() {
            self.register_static(&name);
        }
        self.share(cache)
    }

    /// What a config Source is made of apart from its schedule (see the
    /// `definitions` field); `None` for a built-in or an unknown Source.
    pub fn definition(&self, provider: &str, source: &str) -> Option<&str> {
        self.definitions
            .get(&(provider.to_string(), source.to_string()))
            .map(String::as_str)
    }

    /// Every Source a line calls exists: a `data` line's and a provider's
    /// `result` and `return`. Naming one that does not is a load error, naming
    /// what is missing. So is a ring of Sources that call each other: every
    /// call is named in a line, so the ring is known before anything runs.
    fn check_calls(&self, providers: &toml::Table) -> Vec<String> {
        // A Source whose table is there but did not build has its own error.
        let declared = |provider: &str, source: &str| {
            providers
                .get(provider)
                .and_then(|p| p.get("sources"))
                .and_then(|s| s.get(source))
                .is_some()
        };
        let missing = |provider: &str, source: &str| -> Option<String> {
            if declared(provider, source) {
                return None;
            }
            if !self.providers.contains_key(provider) {
                return Some(format!("daemon.providers.{provider}: no such provider"));
            }
            (!self
                .sources
                .contains_key(&(provider.to_string(), source.to_string())))
            .then(|| format!("daemon.providers.{provider}.sources.{source}: no such source"))
        };
        let mut errors = std::collections::BTreeSet::new();
        let mut edges: std::collections::BTreeMap<(String, String), Vec<(String, String)>> =
            Default::default();
        for (provider, source, called) in &self.data_calls {
            let target = match called {
                Called::Sibling(s) => (provider.clone(), s.clone()),
                Called::Of(p, s) => (p.clone(), s.clone()),
            };
            if let Some(what) = missing(&target.0, &target.1) {
                errors.insert(format!(
                    "[daemon.providers.{provider}.sources.{source}] data: {what}"
                ));
            } else {
                edges
                    .entry((provider.clone(), source.clone()))
                    .or_default()
                    .push(target);
            }
        }
        for (at, owner, called) in self.lines.calls() {
            let (provider, source) = match called {
                Called::Of(p, s) => (p, s),
                Called::Sibling(s) => match owner {
                    Some(p) => (p, s),
                    None => continue,
                },
            };
            if let Some(what) = missing(&provider, &source) {
                errors.insert(format!("{at}: {what}"));
            }
        }
        if let Some(ring) = find_ring(&edges) {
            let named: Vec<String> = ring.iter().map(|(p, s)| format!("{p}.{s}")).collect();
            let (p, s) = &ring[0];
            errors.insert(format!(
                "[daemon.providers.{p}.sources.{s}] data: Sources call each other in a ring: {}",
                named.join(" -> ")
            ));
        }
        errors.into_iter().collect()
    }

    /// Every built-in Source runs on `[daemon] budget` and `[daemon] timeout`;
    /// one whose `return` the files set takes its schedule, failback, decay,
    /// budget and timeout from that line instead. A built-in with no `return` keeps the rest of
    /// what it declares: the `_` lines apply to config Sources only.
    fn retune_builtins(
        &mut self,
        config: &Config,
        builtins: &[(String, String)],
        retunes: &[Retune],
    ) -> Vec<String> {
        let budget = config.daemon().budget();
        let timeout = config.daemon().timeout();
        let mut errors = Vec::new();
        for (provider, source) in builtins {
            let line = retunes
                .iter()
                .find(|r| &r.provider == provider && &r.source == source);
            self.retune(provider, source, |meta| {
                meta.budget = budget;
                meta.timeout = timeout;
                if let Some(Retune { recorded, .. }) = line {
                    meta.invalidation = recorded.invalidation.clone();
                    meta.failback = recorded.failback;
                    meta.decay = recorded.decay;
                    meta.budget = recorded.budget;
                    meta.timeout = recorded.timeout;
                    meta.nearest = recorded.nearest;
                }
            });
            if line.is_some()
                && let Err(e) = self.check_retuned(provider, source)
            {
                errors.push(e);
            }
        }
        errors
    }

    /// A retuned built-in must still be a Source the registry would have
    /// accepted from code: the checks `register` runs, and the one a `return`
    /// can newly break. A Source with no root has one instance, at `~`, which
    /// has no directory to resolve a relative watch path against, so only an
    /// absolute path is ever watched; a relative one would silently watch
    /// nothing.
    fn check_retuned(&self, provider: &str, source: &str) -> Result<(), String> {
        let at = format!("[daemon.providers.{provider}.sources.{source}]");
        let meta = self
            .providers
            .get(provider)
            .and_then(|p| p.sources.iter().find(|s| s.name == source))
            .expect("a retuned Source is registered");
        let rooted = self
            .source(provider, source)
            .expect("a retuned Source is registered")
            .has_root();
        if !rooted
            && let Some(problem) = crate::provider::one_instance_problem(
                &format!("{provider}.{source}"),
                &meta.invalidation,
                meta.nearest,
            )
        {
            return Err(format!("{at} return: {problem}"));
        }
        self.providers[provider]
            .validate(self.merges_by_default(provider))
            .map_err(|e| format!("{at} return: {e}"))
    }
}

/// What a Source gets for a decorator its `return` does not name: every Source
/// has a `failback`, a `budget` and a `timeout`, with no off switch.
fn engine_defaults(config: &Config) -> EngineDefaults {
    EngineDefaults {
        failback: FailbackConfig {
            reattempts: DEFAULT_FAILBACK_COUNT,
            interval_secs: DEFAULT_FAILBACK_INTERVAL_SECS,
        },
        budget: config.daemon().budget(),
        timeout: config.daemon().timeout(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{
        FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, SourceResult,
    };

    /// `f` where a Source's run executes: on a blocking thread of a runtime,
    /// so a call it makes can start and wait for another run.
    fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async { tokio::task::spawn_blocking(f).await })
            .unwrap()
    }

    /// Run `source` for `at` as the daemon does, [`blocking`].
    fn execute(source: &Arc<dyn Source>, at: &str) -> Result<SourceResult, String> {
        let (source, at) = (Arc::clone(source), at.to_string());
        blocking(move || source.execute(Some(&at)))
    }

    struct FakeSource(SourceMetadata);
    impl Source for FakeSource {
        fn metadata(&self) -> &SourceMetadata {
            &self.0
        }
        fn execute(&self, _path: Option<&str>) -> Result<SourceResult, String> {
            Ok(SourceResult::new())
        }
    }

    struct FakeProvider {
        name: String,
        sources: Vec<SourceMetadata>,
    }
    impl Provider for FakeProvider {
        fn metadata(&self) -> ProviderMetadata {
            ProviderMetadata {
                name: self.name.clone(),
                sources: self.sources.clone(),
            }
        }
        fn sources(&self) -> Vec<Box<dyn Source>> {
            self.sources
                .iter()
                .map(|s| Box::new(FakeSource(s.clone())) as Box<dyn Source>)
                .collect()
        }
    }

    fn ms(name: &str, fields: Vec<&str>) -> SourceMetadata {
        SourceMetadata {
            name: name.into(),
            fields: fields
                .into_iter()
                .map(|n| FieldSchema {
                    name: n.into(),
                    field_type: FieldType::String,
                })
                .collect(),
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 30,
                count: 2,
            },
            failback: FailbackConfig {
                reattempts: 3,
                interval_secs: 30,
            },
            decay: None,
            budget: crate::scheduler::lifecycle::DEFAULT_BUDGET,
            timeout: crate::scheduler::lifecycle::DEFAULT_TIMEOUT,
            nearest: false,
        }
    }

    fn registry_of(name: &str, sources: Vec<SourceMetadata>) -> ProviderRegistry {
        let mut reg = ProviderRegistry::new();
        reg.register(Box::new(FakeProvider {
            name: name.into(),
            sources,
        }))
        .unwrap();
        reg
    }

    #[test]
    fn the_nearest_declared_ancestor_node_answers() {
        let reg = registry_of(
            "p",
            vec![ms("deep", vec!["foo.bar"]), ms("top", vec!["foo"])],
        );
        assert_eq!(reg.sources_for_key("p", "foo.bar.baz"), vec!["deep"]);
        assert_eq!(reg.sources_for_key("p", "foo.bar"), vec!["deep"]);
        assert_eq!(reg.sources_for_key("p", "foo.qux"), vec!["top"]);
        assert_eq!(reg.sources_for_key("p", "foo"), vec!["top"]);
    }

    #[test]
    fn a_leaf_below_a_declared_field_finds_its_source() {
        let reg = registry_of(
            "p",
            vec![ms("refs", vec!["branch"]), ms("diff", vec!["lines_added"])],
        );
        assert_eq!(reg.sources_for_key("p", "branch"), vec!["refs"]);
        assert_eq!(reg.sources_for_key("p", "branch.x"), vec!["refs"]);
        assert_eq!(reg.sources_for_key("p", "lines_added"), vec!["diff"]);
    }

    #[test]
    fn sentinel_sources_answer_for_the_provider_node() {
        let reg = registry_of(
            "mise",
            vec![ms("global", vec!["<tool>"]), ms("project", vec!["<tool>"])],
        );
        assert_eq!(
            reg.sources_for_key("mise", "node"),
            vec!["global", "project"]
        );
        assert_eq!(
            reg.sources_for_key("mise", "node.version"),
            vec!["global", "project"]
        );
    }

    #[test]
    fn a_declared_ancestor_beats_the_sentinels() {
        let reg = registry_of(
            "p",
            vec![ms("named", vec!["a"]), ms("rest", vec!["<field>"])],
        );
        assert_eq!(reg.sources_for_key("p", "a.b"), vec!["named"]);
        assert_eq!(reg.sources_for_key("p", "other"), vec!["rest"]);
    }

    #[test]
    fn an_undeclared_key_without_a_sentinel_finds_nothing() {
        let reg = registry_of("git", vec![ms("refs", vec!["branch"])]);
        assert!(reg.sources_for_key("git", "nonexistent").is_empty());
        assert!(reg.sources_for_key("nope", "branch").is_empty());
    }

    /// Only a provider the config names is a provider with zero Sources; one
    /// that declares Sources and has none is refused, whatever its kind.
    #[test]
    fn a_provider_that_declares_no_sources_is_still_rejected() {
        let mut reg = ProviderRegistry::new();
        let err = reg
            .register(Box::new(FakeProvider {
                name: "x".into(),
                sources: vec![],
            }))
            .unwrap_err();
        assert!(err.contains("declares no sources"), "{err}");
    }

    fn registry_from(user: &str) -> ProviderRegistry {
        let config = crate::config_tests::with_shipped_defaults(user);
        ProviderRegistry::with_config(&config, &Default::default()).unwrap()
    }

    fn invalidation(reg: &ProviderRegistry, provider: &str, source: &str) -> InvalidationStrategy {
        reg.provider_sources(provider)
            .unwrap()
            .iter()
            .find(|s| s.name == source)
            .unwrap_or_else(|| panic!("{provider}.{source} is registered"))
            .invalidation
            .clone()
    }

    #[test]
    fn an_added_source_inherits_the_providers_underscore() {
        let reg = registry_from(
            r#"
[daemon.providers.git.sources._]
return = "{{ result | poll('7s', 2) }}"

[daemon.providers.git.sources.extra]
data = "{{ 1 }}"
result.extra = "{{ data }}"

[daemon.providers.demo.sources.s]
data = "{{ 1 }}"
result.x = "{{ data }}"
"#,
        );
        assert!(
            reg.source("git", "extra").is_some(),
            "joins the built-in provider"
        );
        assert!(reg.source("git", "refs").is_some(), "built-ins stay");
        assert_eq!(
            invalidation(&reg, "git", "extra"),
            InvalidationStrategy::Poll {
                interval_secs: 7,
                count: 2
            },
            "git's own `_` beats the abstract one"
        );
        assert_eq!(
            invalidation(&reg, "demo", "s"),
            InvalidationStrategy::Poll {
                interval_secs: 60,
                count: 12
            },
            "another provider's `_` does not reach it"
        );
        assert_eq!(reg.sources_for_key("git", "extra"), vec!["extra"]);
    }

    #[test]
    fn a_source_with_no_return_gets_the_abstract_schedule() {
        let reg = registry_from(
            "[daemon.providers.demo.sources.s]\ndata = \"{{ 1 }}\"\nresult = \"{{ data }}\"\n",
        );
        let meta = &reg.provider_sources("demo").unwrap()[0];
        assert_eq!(
            meta.invalidation,
            InvalidationStrategy::Poll {
                interval_secs: 60,
                count: 12
            }
        );
        assert_eq!(
            (meta.failback.reattempts, meta.failback.interval_secs),
            (3, 1)
        );
        assert_eq!(
            reg.sources_for_key("demo", "anything"),
            vec!["s"],
            "a whole-node Source"
        );
    }

    /// Give `dir` an mtime well in the past, so a stamp of it is not racy and
    /// a test may move the clock.
    fn age(dir: &std::path::Path) {
        std::fs::File::open(dir)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(60))
            .unwrap();
    }

    /// A kept root is rendered again when a marker on its walk comes or goes:
    /// one created below the root it found, the one at that root removed.
    #[tokio::test(start_paused = true)]
    async fn a_kept_root_is_rendered_again_when_a_marker_on_its_walk_changes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().canonicalize().unwrap().join("p");
        let sub = p.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(p.join(".marker"), "").unwrap();
        let (p_str, sub_str) = (p.to_str().unwrap(), sub.to_str().unwrap());
        let reg = registry_from(
            "[daemon.providers.demo.sources.s]\nroot = \"{{ lup('.marker') | dirname }}\"\ndata = \"{{ 1 }}\"\nresult.x = \"{{ data }}\"\n",
        );
        let at = |d: &str| Ok(Found::At(d.to_string()));

        assert_eq!(reg.instance_root("demo", "s", sub_str), at(p_str));
        assert_eq!(
            reg.kept_root("demo", "s", sub_str),
            Some(Found::At(p_str.into()))
        );
        std::fs::write(sub.join(".marker"), "").unwrap();
        assert_eq!(
            reg.kept_root("demo", "s", sub_str),
            None,
            "sub's mtime moved"
        );
        assert_eq!(reg.instance_root("demo", "s", sub_str), at(sub_str));
        std::fs::remove_file(sub.join(".marker")).unwrap();
        assert_eq!(reg.instance_root("demo", "s", sub_str), at(p_str));
        std::fs::remove_file(p.join(".marker")).unwrap();
        assert_eq!(
            reg.instance_root("demo", "s", sub_str),
            Ok(Found::Nothing),
            "the marker at the root it found is gone"
        );
    }

    /// The nothing a nearest Source's root found is checked like any kept
    /// root: a marker created is found by the next lookup, and with nothing
    /// changed it is kept however long it has been, with no timer.
    #[tokio::test(start_paused = true)]
    async fn a_nearest_sources_nothing_is_rendered_again_when_a_marker_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        // The walk of a root that found nothing goes up to `$HOME`: here, the
        // test's own directory, so no other test's files move its stamps.
        // SAFETY: nextest gives every test its own process, and nothing else
        // in it reads the environment yet.
        unsafe { std::env::set_var("HOME", &base) };
        let sub = base.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        age(&sub);
        age(&base);
        let sub_str = sub.to_str().unwrap();
        let reg = registry_from(
            "[daemon.providers.demo.sources.s]\nroot = \"{{ lup('.root') | dirname }}\"\ndata = \"{{ 1 }}\"\nresult.x = \"{{ data }}\"\nreturn = \"{{ result | nearest }}\"\n",
        );

        assert_eq!(reg.instance_root("demo", "s", sub_str), Ok(Found::Nothing));
        tokio::time::advance(ROOT_RETRY_TTL * 10).await;
        assert_eq!(
            reg.kept_root("demo", "s", sub_str),
            Some(Found::Nothing),
            "kept with nothing changed, past any timer"
        );
        std::fs::write(sub.join(".root"), "").unwrap();
        assert_eq!(
            reg.instance_root("demo", "s", sub_str),
            Ok(Found::At(sub_str.to_string())),
            "a marker created is found at once"
        );
    }

    /// A change the walk's stamps do not see (here, what the line reads from
    /// a file rewritten in place) is kept while its instance lives, and looked
    /// up again once the instance it resolved to is forgotten.
    #[tokio::test(start_paused = true)]
    async fn a_kept_root_is_re_resolved_once_its_instance_is_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let sub = base.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let (base_str, sub_str) = (base.to_str().unwrap(), sub.to_str().unwrap());
        std::fs::write(sub.join("root.txt"), base_str).unwrap();
        let reg = registry_from(
            "[daemon.providers.demo.sources.s]\nroot = \"{{ 'root.txt' | read }}\"\ndata = \"{{ 1 }}\"\nresult.x = \"{{ data }}\"\n",
        );

        assert_eq!(
            reg.instance_root("demo", "s", sub_str),
            Ok(Found::At(base_str.to_string()))
        );
        std::fs::write(sub.join("root.txt"), sub_str).unwrap();
        assert_eq!(
            reg.instance_root("demo", "s", sub_str),
            Ok(Found::At(base_str.to_string())),
            "the answer is kept while its instance lives"
        );
        reg.forget_roots("demo", "s", Some("/elsewhere"));
        assert_eq!(
            reg.instance_root("demo", "s", sub_str),
            Ok(Found::At(base_str.to_string())),
            "another instance going cold says nothing about this one"
        );
        reg.forget_roots("demo", "s", Some(base_str));
        assert_eq!(
            reg.instance_root("demo", "s", sub_str),
            Ok(Found::At(sub_str.to_string())),
            "once the instance it resolved to is gone, the root is looked up again"
        );
    }

    /// A cascading Source's nothing is its instance at `~`, cleared when that
    /// is evicted; a nearest Source's is no instance's, so an old one is swept
    /// when a new one is kept.
    #[tokio::test(start_paused = true)]
    async fn an_old_unclaimed_nothing_is_swept_when_another_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let (a, b, c) = (base.join("a"), base.join("b"), base.join("c"));
        for d in [&a, &b, &c] {
            std::fs::create_dir_all(d).unwrap();
        }
        let (a, b, c) = (
            a.to_str().unwrap(),
            b.to_str().unwrap(),
            c.to_str().unwrap(),
        );
        let root = "root = \"{{ lup('.root') | dirname }}\"\ndata = \"{{ 1 }}\"\nresult.x = \"{{ data }}\"\n";
        let reg = registry_from(&format!(
            "[daemon.providers.casc.sources.s]\n{root}\n\
             [daemon.providers.near.sources.s]\n{root}return = \"{{{{ result | nearest }}}}\"\n"
        ));

        assert_eq!(reg.instance_root("casc", "s", a), Ok(Found::Nothing));
        assert_eq!(reg.instance_root("near", "s", b), Ok(Found::Nothing));
        assert_eq!(reg.roots.len(), 2);
        tokio::time::advance(UNCLAIMED_TTL).await;
        assert_eq!(reg.instance_root("near", "s", c), Ok(Found::Nothing));
        assert_eq!(
            reg.roots.len(),
            2,
            "b's went when c's was kept; the cascading a's stays"
        );
        assert!(
            reg.roots
                .contains_key(&("casc".into(), "s".into(), a.into()))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn lapsed_root_errors_are_swept_when_another_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let reg = registry_from(
            "[daemon.providers.demo.sources.s]\nroot = \"{{ exec('false') }}\"\ndata = \"{{ 1 }}\"\nresult.x = \"{{ data }}\"\n",
        );
        let ask = |name: &str| {
            let d = base.join(name);
            std::fs::create_dir_all(&d).unwrap();
            reg.instance_root("demo", "s", d.to_str().unwrap())
        };

        assert!(ask("a").is_err());
        assert!(ask("b").is_err());
        assert_eq!(reg.root_errors.len(), 2);
        tokio::time::advance(ROOT_RETRY_TTL).await;
        assert!(ask("c").is_err());
        assert_eq!(
            reg.root_errors.len(),
            1,
            "the lapsed answers for a and b went when c's was kept"
        );
    }

    #[test]
    fn the_runs_survive_a_reload() {
        let src =
            "[daemon.providers.demo.sources.s]\ndata = \"{{ 1 }}\"\nresult.x = \"{{ data }}\"\n";
        let cache = Arc::new(crate::cache::Cache::new());
        let before = registry_from(src).share(&cache);
        let after = registry_from(src).adopt(&before, &cache);
        assert!(
            Arc::ptr_eq(before.runs(), after.runs()),
            "a caller on the new registry finds the run the old one has under way"
        );
    }

    #[test]
    fn a_retune_replaces_nearest_with_the_lines_setting() {
        let reg = registry_from(
            "[daemon.providers.git.sources.diff]\nreturn = \"{{ result | poll('15s') }}\"\n\
             [daemon.providers.mise.sources.tools]\nreturn = \"{{ result | poll('15s') | nearest }}\"\n",
        );
        assert!(
            !source_meta(&reg, "git", "diff").nearest,
            "git.diff declared nearest, and its own line does not say it"
        );
        assert!(source_meta(&reg, "mise", "tools").nearest);
        assert!(
            source_meta(&reg, "git", "head").nearest,
            "a Source with no return keeps what it declares"
        );
    }

    #[test]
    fn built_in_sources_with_a_root_declare_how_their_instances_combine() {
        let reg = ProviderRegistry::shipped(&Default::default());
        let mut rooted = Vec::new();
        for (provider, metas) in reg.registered_sources() {
            for sm in metas {
                let source = reg.source(&provider, &sm.name).unwrap();
                if source.has_root() {
                    rooted.push((provider.clone(), sm.name.clone(), sm.nearest));
                } else {
                    assert!(!sm.nearest, "{provider}.{}: no root to be nearest", sm.name);
                }
            }
        }
        assert!(!rooted.is_empty());
        for (provider, source, nearest) in rooted {
            let want = matches!(provider.as_str(), "git" | "direnv" | "python" | "terraform");
            assert_eq!(nearest, want, "{provider}.{source}");
        }
    }

    #[test]
    fn a_providers_sources_that_find_their_root_alike_share_one_lookup() {
        let reg = ProviderRegistry::shipped(&Default::default());
        let keys: std::collections::HashSet<String> = reg
            .provider_sources("git")
            .unwrap()
            .iter()
            .map(|sm| reg.source("git", &sm.name).unwrap().root_key())
            .collect();
        assert_eq!(keys.len(), 1, "git's Sources walk to the repository alike");
    }

    #[test]
    fn a_failing_root_line_is_not_re_run_for_every_request() {
        let dir = tempfile::tempdir().unwrap();
        let runs = dir.path().join("runs");
        let reg = registry_from(&format!(
            "[daemon.providers.demo.sources.s]\nroot = \"{{{{ exec('echo x >> {}; false') }}}}\"\ndata = \"{{{{ 1 }}}}\"\nresult.x = \"{{{{ data }}}}\"\n",
            runs.display()
        ));
        let first = reg.instance_root("demo", "s", "/tmp").unwrap_err();
        let second = reg.instance_root("demo", "s", "/tmp").unwrap_err();
        assert_eq!(first, second, "the same error is answered again");
        assert_eq!(
            std::fs::read_to_string(&runs).unwrap().lines().count(),
            1,
            "the line ran once for two requests"
        );
    }

    fn source_meta(reg: &ProviderRegistry, provider: &str, source: &str) -> SourceMetadata {
        reg.provider_sources(provider)
            .unwrap()
            .iter()
            .find(|s| s.name == source)
            .unwrap_or_else(|| panic!("{provider}.{source} is registered"))
            .clone()
    }

    #[test]
    fn config_sources_are_counted_as_registered_not_as_written() {
        let reg = registry_from(
            r#"
[daemon.providers.demo.sources._]
data = "{{ 1 }}"
result.x = "{{ data }}"

[daemon.providers.demo.sources.s]
return = "{{ result | poll('5s') }}"

[daemon.providers.git.sources.diff]
return = "{{ result | poll('15s') }}"
"#,
        );
        // The shipped files' Sources are config Sources too.
        let shipped = registry_from("").config_source_count();
        assert_eq!(
            reg.config_source_count(),
            shipped + 1,
            "demo.s takes its lines from `_`; the retuned built-in and `_` itself are not config Sources"
        );
        assert_eq!(
            ProviderRegistry::with_config(&Config::default(), &Default::default())
                .unwrap()
                .config_source_count(),
            0
        );
    }

    #[test]
    fn a_builtin_source_is_retuned_by_its_return_line() {
        let reg = registry_from(
            "[daemon.providers.git.sources.diff]\nreturn = \"{{ result | poll('15s', 8) }}\"\n",
        );
        assert_eq!(
            invalidation(&reg, "git", "diff"),
            InvalidationStrategy::Poll {
                interval_secs: 15,
                count: 8
            },
            "the line replaces the schedule whole"
        );
        // The Source the scheduler holds says the same, and still runs.
        let held = reg.source("git", "diff").unwrap();
        assert_eq!(
            held.metadata().invalidation,
            invalidation(&reg, "git", "diff")
        );
        // The others keep theirs.
        let stock = ProviderRegistry::shipped(&Default::default());
        assert_eq!(
            invalidation(&reg, "git", "refs"),
            invalidation(&stock, "git", "refs")
        );
        let names =
            |m: &SourceMetadata| m.fields.iter().map(|f| f.name.clone()).collect::<Vec<_>>();
        assert_eq!(
            names(held.metadata()),
            names(&source_meta(&stock, "git", "diff")),
            "same fields: only the schedule moved"
        );
    }

    #[test]
    fn a_builtin_source_without_a_return_keeps_its_rust_schedule() {
        // The shipped abstract `_` says `poll('60s', 12)`; it applies to config
        // Sources only, so watch-driven built-ins stay watch-driven.
        let reg = registry_from("[daemon.providers.git.sources.diff]\n");
        let stock = ProviderRegistry::shipped(&Default::default());
        for (provider, source) in [("git", "diff"), ("git", "refs"), ("mise", "tools")] {
            let (got, want) = (
                source_meta(&reg, provider, source),
                source_meta(&stock, provider, source),
            );
            assert_eq!(got.invalidation, want.invalidation, "{provider}.{source}");
            assert_eq!(got.failback, want.failback, "{provider}.{source}");
            assert_eq!(got.decay, want.decay, "{provider}.{source}");
        }
        assert!(matches!(
            invalidation(&reg, "git", "refs"),
            InvalidationStrategy::Watch { .. }
        ));
    }

    #[test]
    fn a_builtin_source_takes_the_engine_failback_and_budget() {
        // A return that names neither: the engine's failback, the daemon's
        // budget and timeout.
        let reg = registry_from(
            "[daemon]\nbudget = \"30ms\"\ntimeout = \"4s\"\n\
             [daemon.providers.git.sources.diff]\nreturn = \"{{ result | poll('15s') }}\"\n",
        );
        let diff = source_meta(&reg, "git", "diff");
        assert_ne!(
            ProviderRegistry::shipped(&Default::default())
                .provider_sources("git")
                .unwrap()
                .iter()
                .find(|s| s.name == "diff")
                .unwrap()
                .failback,
            diff.failback,
            "git.diff declares its own failback"
        );
        assert_eq!(
            (diff.failback.reattempts, diff.failback.interval_secs),
            (
                crate::scheduler::lifecycle::DEFAULT_FAILBACK_COUNT,
                crate::scheduler::lifecycle::DEFAULT_FAILBACK_INTERVAL_SECS
            )
        );
        assert_eq!(diff.budget, std::time::Duration::from_millis(30));
        assert_eq!(diff.timeout, std::time::Duration::from_secs(4));
        // Its own line wins when it names them.
        let reg = registry_from(
            "[daemon.providers.git.sources.diff]\n\
             return = \"{{ result | failback('2s', 5) | budget('7ms') }}\"\n",
        );
        let diff = source_meta(&reg, "git", "diff");
        assert_eq!(
            (diff.failback.reattempts, diff.failback.interval_secs),
            (5, 2)
        );
        assert_eq!(diff.budget, std::time::Duration::from_millis(7));
        // And the budget reaches the built-ins that have no return.
        let reg = registry_from("[daemon]\nbudget = \"30ms\"\n");
        assert_eq!(
            source_meta(&reg, "git", "refs").budget,
            std::time::Duration::from_millis(30)
        );
    }

    #[test]
    fn a_builtins_super_reaches_the_prototypes_like_a_config_sources() {
        // own -> git's `_` -> the abstract `_` (the shipped poll('60s', 12)).
        let reg = registry_from(
            r#"
[daemon.providers.git.sources._]
return = "{{ result | failback('3s', 7) | super }}"

[daemon.providers.git.sources.diff]
return = "{{ result | super }}"
"#,
        );
        let diff = source_meta(&reg, "git", "diff");
        assert_eq!(
            diff.invalidation,
            InvalidationStrategy::Poll {
                interval_secs: 60,
                count: 12
            },
            "the abstract `_`, two levels up"
        );
        assert_eq!(
            (diff.failback.reattempts, diff.failback.interval_secs),
            (7, 3),
            "git's `_`, one level up, beats the abstract failback('1s', 3)"
        );
        // A nearer call of a decorator beats the prototype's.
        let reg = registry_from(
            "[daemon.providers.git.sources.diff]\n\
             return = \"{{ result | poll('9s', 2) | super }}\"\n",
        );
        assert_eq!(
            invalidation(&reg, "git", "diff"),
            InvalidationStrategy::Poll {
                interval_secs: 9,
                count: 2
            }
        );
        // A sibling without a return of its own is untouched by the `_` lines.
        assert!(matches!(
            invalidation(&reg, "git", "refs"),
            InvalidationStrategy::Watch { .. }
        ));
    }

    #[test]
    fn a_comb_read_whose_root_fails_fails_the_data_render() {
        let cache = Arc::new(Cache::new());
        let reg = registry_from(
            r#"
[daemon.providers.q.sources.s]
root = "rel"
data = "{{ 1 }}"
result.x = "{{ data }}"

[daemon.providers.p.sources.s]
data = "{{ comb.q.x }}"
result.y = "{{ data }}"
"#,
        )
        .share(&cache);
        let err = reg
            .source("p", "s")
            .unwrap()
            .execute(Some("/tmp"))
            .unwrap_err();
        assert!(err.starts_with("p.s: data: comb.q: "), "{err}");
        assert!(
            err.contains("q.s: root: must render an absolute directory"),
            "{err}"
        );
    }

    #[test]
    #[should_panic(expected = "bound once")]
    fn a_second_bind_is_refused() {
        let cache = Arc::new(Cache::new());
        let registry = ProviderRegistry::shipped(&Default::default()).share(&cache);
        registry.bind(&cache);
    }

    #[test]
    fn a_config_named_provider_has_zero_sources() {
        let mut reg = ProviderRegistry::new();
        assert!(reg.register_config_named("aws"));
        assert!(reg.is_registered("aws"));
        assert!(reg.provider_sources("aws").unwrap().is_empty());
        assert!(!reg.register_config_named("aws"), "once");
    }

    #[test]
    fn register_rejects_duplicate_provider() {
        let mut reg = ProviderRegistry::new();
        let p1 = FakeProvider {
            name: "x".into(),
            sources: vec![ms("a", vec!["f"])],
        };
        let p2 = FakeProvider {
            name: "x".into(),
            sources: vec![ms("b", vec!["g"])],
        };
        reg.register(Box::new(p1)).unwrap();
        assert!(reg.register(Box::new(p2)).is_err());
    }

    #[test]
    fn source_lookup_returns_arc() {
        let mut reg = ProviderRegistry::new();
        let p = FakeProvider {
            name: "mise".into(),
            sources: vec![ms("global", vec!["python"])],
        };
        reg.register(Box::new(p)).unwrap();
        assert!(reg.source("mise", "global").is_some());
        assert!(reg.source("mise", "project").is_none());
        assert!(reg.source("unknown", "global").is_none());
    }

    #[test]
    fn static_registration_blocked_by_real_provider() {
        let mut reg = ProviderRegistry::new();
        let p = FakeProvider {
            name: "hostname".into(),
            sources: vec![ms("name", vec!["value"])],
        };
        reg.register(Box::new(p)).unwrap();
        assert!(!reg.register_static("hostname"));
        assert!(reg.register_static("myvar"));
        assert!(reg.is_static_namespace("myvar"));
    }

    /// `failback` takes the interval first; the count-first order is a load
    /// error naming the table, on a config Source and a built-in alike, never
    /// read the other way round.
    #[test]
    fn the_count_first_failback_order_is_a_load_error_naming_its_table() {
        let load = |text: &str| {
            let config = crate::config_tests::with_shipped_defaults(text);
            ProviderRegistry::load(&config, &Default::default()).errors
        };
        let order = "failback(): takes the interval first, as a string like '1s', then the count: \
                     failback('1s', 3)";
        for (text, table) in [
            (
                "[daemon.providers.p.sources.s]\ndata = \"{{ 1 }}\"\nresult.a = \"{{ data }}\"\n\
                 return = \"{{ result | failback(3, '1s') }}\"\n",
                "[daemon.providers.p.sources.s]",
            ),
            (
                "[daemon.providers.git.sources.diff]\n\
                 return = \"{{ result | poll('15s', 8) | failback(5, '2s') }}\"\n",
                "[daemon.providers.git.sources.diff]",
            ),
        ] {
            let errors = load(text);
            assert!(
                errors.len() == 1 && errors[0].contains(table) && errors[0].ends_with(order),
                "{errors:?}"
            );
        }
        assert!(
            load(
                "[daemon.providers.git.sources.diff]\n\
             return = \"{{ result | poll('15s', 8) | failback('2s', 5) }}\"\n"
            )
            .is_empty()
        );
    }

    #[test]
    fn enabled_on_a_name_nothing_gives_a_provider_is_a_load_error() {
        let load = |text: &str| {
            let config = crate::config_tests::with_shipped_defaults(text);
            ProviderRegistry::load(&config, &Default::default()).errors
        };
        assert_eq!(
            load("[daemon.providers.gti]\nenabled = false\n"),
            vec![
                "config(/user.toml): [daemon.providers.gti] enabled: no provider is named 'gti' \
                 (no built-in, Source or template gives it one)"
            ]
        );
        // Named by a built-in, a client template, or a table of its own lines.
        assert!(load("[daemon.providers.git]\nenabled = false\n").is_empty());
        assert!(
            load("[client.providers.mine.template]\nx = \"{{ 1 }}\"\n[daemon.providers.mine]\nenabled = false\n")
                .is_empty()
        );
        assert!(
            load("[daemon.providers.lit]\nenabled = false\nresult = \"{{ {} }}\"\n").is_empty()
        );
    }

    #[test]
    fn a_call_to_an_unknown_provider_or_source_is_a_load_error() {
        let config = crate::config::Config::from_toml(
            "[daemon.providers.p.sources.s]\n\
             data = \"{{ daemon.providers.nope.sources.t() ~ daemon.providers.p.sources.zip() \
             ~ sources.gone() }}\"\n\
             result.a = \"{{ data }}\"\n\
             [daemon.providers.q]\n\
             result = \"{{ merge(sources) | merge(daemon.providers.nope.sources.t()) \
             | merge(sources.nope()) }}\"\n\
             return = \"{{ result | merge(daemon.providers.p.sources.zap()) }}\"\n",
        );
        let errors = ProviderRegistry::load(&config, &Default::default()).errors;
        assert_eq!(
            errors,
            vec![
                "config(<in memory>): [daemon.providers.p.sources.s] data: daemon.providers.nope: \
                 no such provider",
                "config(<in memory>): [daemon.providers.p.sources.s] data: \
                 daemon.providers.p.sources.gone: no such source",
                "config(<in memory>): [daemon.providers.p.sources.s] data: \
                 daemon.providers.p.sources.zip: no such source",
                "config(<in memory>): [daemon.providers.q] result: daemon.providers.nope: no such \
                 provider",
                "config(<in memory>): [daemon.providers.q] result: \
                 daemon.providers.q.sources.nope: no such source",
                "config(<in memory>): [daemon.providers.q] return: \
                 daemon.providers.p.sources.zap: no such source",
            ]
        );
    }

    const RING: &str = "[daemon.providers.cyc.sources.a]\n\
         data = \"{{ sources.b().xb }}\"\n\
         result.xa = \"{{ data }}\"\n\
         [daemon.providers.cyc.sources.b]\n\
         data = \"{{ daemon.providers.other.sources.c().xc }}\"\n\
         result.xb = \"{{ data }}\"\n\
         [daemon.providers.other.sources.c]\n\
         data = \"{{ daemon.providers.cyc.sources.a().xa }}\"\n\
         result.xc = \"{{ data }}\"\n";

    #[test]
    fn sources_that_call_each_other_in_a_ring_are_a_load_error() {
        let config = crate::config::Config::from_toml(RING);
        let errors = ProviderRegistry::load(&config, &Default::default()).errors;
        assert_eq!(
            errors,
            vec![
                "config(<in memory>): [daemon.providers.cyc.sources.a] data: Sources call each \
                 other in a ring: cyc.a -> cyc.b -> other.c -> cyc.a"
            ]
        );
    }

    #[test]
    fn a_ring_that_got_past_the_load_check_fails_when_run() {
        let config = crate::config::Config::from_toml(RING);
        let mut registry = ProviderRegistry::shipped(&Default::default());
        let (built, errors) = registry.config_sources(&config, &Default::default());
        assert_eq!(errors, Vec::<String>::new(), "the lines are fine");
        for (name, sources) in built.providers {
            registry.register_config_sources(&name, sources).unwrap();
        }
        let cache = Arc::new(Cache::new());
        let registry = registry.share(&cache);
        let err = blocking(move || {
            crate::provider::source_call::run_call(&registry, &cache, "cyc", "a", None)
        })
        .unwrap_err();
        assert!(err.contains("sources: cyc.a calls itself"), "{err}");
        assert!(err.matches("(in <expression>").count() <= 1, "{err}");
    }

    #[test]
    fn a_call_on_the_read_path_never_renders_a_root() {
        let dir = tempfile::tempdir().unwrap();
        // Counted outside the directory the root is asked about: a file the
        // line created there would move the stamp its kept root is checked
        // by, and the root would be rendered again. The Source is nearest, so
        // its one instance is the root found for `dir`: a cascade would ask
        // the directories above, the system's temporary directory among them,
        // whose stamps every other test moves.
        let counts = tempfile::tempdir().unwrap();
        let renders = counts.path().join("renders");
        age(dir.path());
        let config = crate::config::Config::from_toml(&format!(
            "[daemon.providers.p.sources.s]\n\
             root = \"{{{{ exec(['sh', '-c', 'echo r >> {}; echo {}']) | trim }}}}\"\n\
             data = \"{{{{ 1 }}}}\"\n\
             result.a = \"{{{{ data }}}}\"\n\
             return = \"{{{{ result | nearest }}}}\"\n",
            renders.display(),
            dir.path().canonicalize().unwrap().display(),
        ));
        let registry = ProviderRegistry::with_config(&config, &Default::default()).unwrap();
        let cache = Arc::new(Cache::new());
        let registry = registry.share(&cache);
        use crate::provider::source_call::{run_call, source_call};
        let at = dir.path().canonicalize().unwrap();
        let at = at.to_str().unwrap().to_string();
        let skipped = source_call(&registry, &cache, "p", "s", Some(&at)).unwrap();
        assert!(skipped.is_none(), "an unresolved instance is a miss");
        assert!(!renders.exists(), "the root line was not rendered");
        let ran = blocking({
            let (registry, cache, at) = (registry.clone(), cache.clone(), at.clone());
            move || run_call(&registry, &cache, "p", "s", Some(&at)).unwrap()
        });
        assert!(
            ran.is_some() && renders.exists(),
            "a call that may run does"
        );
        let skipped = source_call(&registry, &cache, "p", "s", Some(&at)).unwrap();
        assert!(skipped.is_some(), "and then the root is kept, and read");
    }

    #[test]
    fn a_source_that_did_not_build_is_not_reported_again_by_each_call() {
        let config = crate::config::Config::from_toml(
            "[daemon.providers.p.sources.a]\n\
             data = \"{{ 1 }}\"\n\
             result.x = \"{{ nowhere }}\"\n\
             [daemon.providers.q.sources.b]\n\
             data = \"{{ daemon.providers.p.sources.a() }}\"\n\
             result.y = \"{{ data }}\"\n",
        );
        let errors = ProviderRegistry::load(&config, &Default::default()).errors;
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].starts_with("config(<in memory>): [daemon.providers.p.sources.a] "),
            "{errors:?}"
        );
    }

    #[test]
    fn callers_waiting_for_a_failing_cold_source_share_its_one_run() {
        use crate::provider::source_call::run_call;
        let dir = tempfile::tempdir().unwrap();
        let count = dir.path().join("runs");
        // The run waits for `go`, which the test makes once every caller holds
        // the instance's one run: one started it, two took it.
        let go = dir.path().join("go");
        let config = crate::config::Config::from_toml(&format!(
            "[daemon.providers.p.sources.t]\n\
             data = \"{{{{ exec(['sh', '-c', 'while [ ! -e {} ]; do sleep 0.01; done; echo run >> {}; exit 3']) }}}}\"\n\
             result.v = \"{{{{ data }}}}\"\n",
            go.display(),
            count.display()
        ));
        let cache = Arc::new(Cache::new());
        let registry = ProviderRegistry::with_config(&config, &Default::default())
            .unwrap()
            .share(&cache);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let start = Arc::new(std::sync::Barrier::new(3));
        let callers: Vec<_> = (0..3)
            .map(|_| {
                let (registry, cache, start) = (registry.clone(), cache.clone(), start.clone());
                let runtime = rt.handle().clone();
                std::thread::spawn(move || {
                    // A caller is a Source's run, on a blocking thread of the
                    // daemon's runtime.
                    let _runtime = runtime.enter();
                    start.wait();
                    run_call(&registry, &cache, "p", "t", None)
                })
            })
            .collect();
        let key = crate::provider::source_call::instance_key("p", "t", None);
        let holders = || registry.runs().callers(&key);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while holders() != Some(3) {
            assert!(
                std::time::Instant::now() < deadline,
                "three callers hold the run: {:?}",
                holders()
            );
            std::thread::yield_now();
        }
        std::fs::write(&go, "").unwrap();
        for caller in callers {
            assert!(
                caller.join().unwrap().unwrap().is_none(),
                "a failed instance is a miss, not an error"
            );
        }
        assert_eq!(
            std::fs::read_to_string(&count).unwrap().lines().count(),
            1,
            "the callers that waited did not each run it again"
        );
        assert!(
            registry.runs().in_flight().is_empty(),
            "no run is kept past its callers"
        );
    }

    /// The instances a demand for `p.a` is a demand for, as source names.
    fn demanded_with(
        registry: &ProviderRegistry,
        source: &str,
        root: &Option<String>,
    ) -> Vec<String> {
        let mut names: Vec<String> = registry
            .with_callees("p", &[crate::query::Instance::new(source, root.as_deref())])
            .into_iter()
            .map(|(p, i)| format!("{p}.{}", i.source))
            .collect();
        names.sort();
        names
    }

    fn calling_registry(flag: &std::path::Path) -> (Arc<ProviderRegistry>, Arc<Cache>) {
        let config = crate::config::Config::from_toml(&format!(
            "[daemon.providers.p.sources.b]\n\
             data = \"{{{{ 1 }}}}\"\n\
             result.vb = \"{{{{ data }}}}\"\n\
             [daemon.providers.p.sources.c]\n\
             data = \"{{{{ 2 }}}}\"\n\
             result.vc = \"{{{{ data }}}}\"\n\
             [daemon.providers.p.sources.a]\n\
             data = \"{{{{ sources.b().vb if 'b' in exec(['cat', '{}']) else sources.c().vc }}}}\"\n\
             result.va = \"{{{{ data }}}}\"\n",
            flag.display()
        ));
        let cache = Arc::new(Cache::new());
        let registry = ProviderRegistry::with_config(&config, &Default::default())
            .unwrap()
            .share(&cache);
        (registry, cache)
    }

    #[test]
    fn a_caller_demands_only_what_its_last_run_called() {
        let dir = tempfile::tempdir().unwrap();
        let flag = dir.path().join("flag");
        std::fs::write(&flag, "b").unwrap();
        let (registry, _cache) = calling_registry(&flag);
        let at = dir.path().to_str().unwrap();
        let root = crate::query::address_of(at);
        let a = registry.source("p", "a").unwrap();

        execute(&a, at).unwrap();
        assert_eq!(demanded_with(&registry, "a", &root), ["p.a", "p.b"]);

        // The branch that called `b` is not taken: `b` is no longer demanded
        // by a read of `a`.
        std::fs::write(&flag, "c").unwrap();
        execute(&a, at).unwrap();
        assert_eq!(demanded_with(&registry, "a", &root), ["p.a", "p.c"]);
    }

    #[test]
    fn a_run_that_fails_replaces_what_the_caller_called_too() {
        let dir = tempfile::tempdir().unwrap();
        let flag = dir.path().join("flag");
        std::fs::write(&flag, "b").unwrap();
        let (registry, _cache) = calling_registry(&flag);
        let at = dir.path().to_str().unwrap();
        let root = crate::query::address_of(at);
        let a = registry.source("p", "a").unwrap();
        execute(&a, at).unwrap();
        assert_eq!(demanded_with(&registry, "a", &root), ["p.a", "p.b"]);

        // `cat` of a file that is gone fails the line before any call is made.
        std::fs::remove_file(&flag).unwrap();
        assert!(execute(&a, at).is_err());
        assert_eq!(demanded_with(&registry, "a", &root), ["p.a"]);
    }

    #[test]
    fn what_an_instance_called_is_forgotten_when_it_is_evicted() {
        let dir = tempfile::tempdir().unwrap();
        let flag = dir.path().join("flag");
        std::fs::write(&flag, "b").unwrap();
        let (registry, _cache) = calling_registry(&flag);
        let at = dir.path().to_str().unwrap();
        let root = crate::query::address_of(at);
        execute(&registry.source("p", "a").unwrap(), at).unwrap();
        assert_eq!(registry.callers_of().len(), 1);

        registry.forget_callees("p", "a", Some("/elsewhere"));
        assert_eq!(registry.callers_of().len(), 1, "another instance's");
        registry.forget_callees("p", "a", root.as_deref());
        assert!(registry.callers_of().is_empty());
        assert_eq!(demanded_with(&registry, "a", &root), ["p.a"]);
    }

    /// A call that finds the instance in failback backoff (a run of it has
    /// just failed) does not run it again: it is a miss.
    #[test]
    fn a_call_does_not_run_an_instance_in_backoff() {
        use crate::provider::source_call::{instance_key, run_call};
        let config = crate::config::Config::from_toml(
            "[daemon.providers.p.sources.t]\n\
             data = \"{{ 1 }}\"\n\
             result.v = \"{{ data }}\"\n",
        );
        let home = tempfile::tempdir().unwrap();
        let vars = HashMap::from([("HOME".to_string(), home.path().display().to_string())]);
        let cache = Arc::new(Cache::new());
        let registry = ProviderRegistry::with_config(&config, &vars)
            .unwrap()
            .share(&cache);
        registry
            .failures
            .lock()
            .unwrap()
            .entry(instance_key("p", "t", None))
            .or_insert_with(|| {
                crate::scheduler::FailureState::new(1, std::time::Duration::from_secs(3600))
            })
            .record_failure();

        assert!(
            run_call(&registry, &cache, "p", "t", None)
                .unwrap()
                .is_none()
        );
        // A run would have succeeded and stored its record.
        assert!(
            !cache.has_provider_records_for("p", None, "t"),
            "the call did not run the instance that had just failed"
        );
    }
}
