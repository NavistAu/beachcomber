use crate::boundaries::library::LibloadingLoader;
use crate::cache::{Cache, Layers};
use crate::config::Config;
use crate::engine::PluginSet;
use crate::provider::config_source::{ConfigSource, DaemonRead, lint_lines};
use crate::provider::{
    FailbackConfig, InvalidationStrategy, Provider, ProviderMetadata, Source, SourceMetadata,
    SourceResult, Value, is_sentinel,
};
use crate::scheduler::lifecycle::{DEFAULT_FAILBACK_COUNT, DEFAULT_FAILBACK_INTERVAL_SECS};
use crate::shape::Retune;
use crate::shape::decorators::EngineDefaults;
use dashmap::DashMap;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock, Weak};

/// What [`ProviderRegistry::load`] made of a configuration.
pub struct ConfigLoad {
    /// The providers that did load; trustworthy only when `errors` is empty.
    pub registry: ProviderRegistry,
    pub errors: Vec<String>,
}

/// What a configuration declares: the config Sources, by provider, and the
/// built-in Sources a `return` retunes.
#[derive(Default)]
pub struct ConfigSources {
    pub providers: Vec<(String, Vec<ConfigSource>)>,
    pub retunes: Vec<Retune>,
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
    /// Never executes a Source. `dir` is a directory; `$HOME` addresses the
    /// global entry, as `QueryPlan` normalises it. It reads every instance up
    /// each Source's chain from `dir`, as a whole-provider read does.
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
        let instances = crate::query::instances_for(
            &registry,
            &plan.provider,
            &crate::query::SourceDemand::All,
            plan.requested_path.as_deref(),
        )?;
        Ok(cache
            .read_merged(
                &plan.provider,
                plan.requested_path.as_deref(),
                &crate::query::roots_of(&instances),
                &registry,
                Layers::All,
            )
            .map(|merged| merged.fields))
    }
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

    fn watched_files(&self, root: Option<&str>) -> Vec<std::path::PathBuf> {
        self.inner.watched_files(root)
    }

    fn read_always(&self) -> bool {
        self.inner.read_always()
    }
}

/// How long a failed `root` line's error is answered again without re-running it.
const ROOT_ERROR_TTL: std::time::Duration = std::time::Duration::from_secs(5);

pub struct ProviderRegistry {
    /// (provider_name, source_name) → Arc-wrapped Source trait object.
    sources: HashMap<(String, String), Arc<dyn Source>>,
    /// Provider metadata snapshot, indexed by provider name.
    providers: HashMap<String, ProviderMetadata>,
    /// (provider_name, declared dotted field) → source_name, sentinels excluded.
    /// Built once at registration.
    declared_sources: HashMap<(String, String), String>,
    /// provider_name → Sources declaring a sentinel field (`<tool>`), in
    /// declaration order. They answer for the provider node.
    sentinel_sources: HashMap<String, Vec<String>>,
    /// Static namespace names registered via `comb put`. Orthogonal to the source model.
    static_names: DashMap<String, ()>,
    /// The `root` of a config Source, by (provider, source, directory): the
    /// line is rendered once per directory (it may run a command) and kept
    /// until the registry is replaced.
    roots: DashMap<(String, String, String), Option<String>>,
    /// A `root` line that failed, by (provider, source, directory), with when
    /// and what it said: answered again for [`ROOT_ERROR_TTL`] rather than
    /// re-run, so a slow failing line costs its timeout once, not per request.
    root_errors: DashMap<(String, String, String), (std::time::Instant, String)>,
    /// Consecutive failures and failback backoff, per instance. The scheduler
    /// and the inline cold-miss path both record here, so an instance in
    /// backoff is skipped by either.
    pub(crate) failures: crate::scheduler::SourceFailureMap,
    /// What every config Source reads `comb.<p>` through.
    reader: Arc<RegistryReader>,
    /// The config Sources registered, as (provider, source).
    config_sources: HashSet<(String, String)>,
    /// The config Sources with a `root` line, as (provider, source).
    rendered_roots: HashSet<(String, String)>,
}

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
            static_names: DashMap::new(),
            roots: DashMap::new(),
            root_errors: DashMap::new(),
            failures: Arc::default(),
            reader: Arc::new(RegistryReader::default()),
            config_sources: HashSet::new(),
            rendered_roots: HashSet::new(),
        }
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

    /// The kept root of the Source for `dir`, if it has been rendered: never
    /// renders. `Some(root)` is the answer ([`Self::instance_root`]'s).
    pub fn kept_root(&self, provider: &str, source: &str, dir: &str) -> Option<Option<String>> {
        self.roots
            .get(&(provider.to_string(), source.to_string(), dir.to_string()))
            .map(|root| root.clone())
    }

    /// The instance of `provider`'s Source `source` that answers for `dir`:
    /// `Some(root)`, or `None` for the one instance at `~`. A root equal to
    /// `$HOME` is the instance at `~` ([`crate::query::normalise_path`]).
    ///
    /// A config Source renders its `root` line once per directory and the
    /// answer is kept until the instance it names is evicted
    /// ([`Self::forget_roots`]); an error is kept for [`ROOT_ERROR_TTL`] only.
    /// May run a command: callers on an async worker go through
    /// [`Self::renders_root`] and [`Self::kept_root`] first.
    pub fn instance_root(
        &self,
        provider: &str,
        source: &str,
        dir: &str,
    ) -> Result<Option<String>, String> {
        if let Some(kept) = self.kept_root(provider, source, dir) {
            return Ok(kept);
        }
        let src = self
            .source(provider, source)
            .ok_or_else(|| format!("{provider}.{source}: no such source"))?;
        let error_key = (provider.to_string(), source.to_string(), dir.to_string());
        if let Some(failed) = self.root_errors.get(&error_key) {
            if failed.0.elapsed() < ROOT_ERROR_TTL {
                return Err(failed.1.clone());
            }
            drop(failed);
            self.root_errors.remove(&error_key);
        }
        // Two requests that both miss the kept root for a directory at once
        // each render the line, and the later insert replaces the earlier: a
        // duplicate first render, not a wrong answer, since both ask the same
        // question of the same directory.
        let found = src.root(dir);
        if let Err(msg) = &found
            && self.renders_root(provider, source)
        {
            self.root_errors
                .insert(error_key, (std::time::Instant::now(), msg.clone()));
        }
        let root = match found? {
            // A root that is the directory asked about is already canonical.
            Some(root) if root == dir => Some(root),
            Some(root) => crate::query::normalise_path(Some(&root)),
            None => None,
        };
        if self.renders_root(provider, source) {
            self.roots.insert(
                (provider.to_string(), source.to_string(), dir.to_string()),
                root.clone(),
            );
        }
        Ok(root)
    }

    /// Drop the kept roots of `provider`'s Source `source` that resolved to
    /// the instance at `root` (`None` is the instance at `~`): the scheduler
    /// calls this when that instance is evicted, so a root is looked up again
    /// once the instance it named has gone cold. A marker created or removed
    /// while the instance lives is not seen until then.
    pub fn forget_roots(&self, provider: &str, source: &str, root: Option<&str>) {
        self.roots
            .retain(|(p, s, _), kept| !(p == provider && s == source && kept.as_deref() == root));
    }

    /// Register a provider. Validates metadata, builds the field→source and sentinel maps,
    /// and inserts all sources. Returns `Err` if the provider name is already registered,
    /// metadata is invalid, or sources() doesn't match metadata.
    pub fn register(&mut self, provider: Box<dyn Provider>) -> Result<(), String> {
        let meta = provider.metadata();
        meta.validate()?;
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
                self.declared_sources
                    .insert((provider.to_string(), f.name.clone()), sm.name.clone());
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
        meta.validate()?;
        for sm in &meta.sources[first_new..] {
            self.index_fields(provider, sm);
        }
        for source in sources {
            let key = (provider.to_string(), source.metadata().name.clone());
            if source.has_root() {
                self.rendered_roots.insert(key.clone());
            }
            self.config_sources.insert(key.clone());
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
    ) -> Result<ConfigSources, Vec<String>> {
        let Some(providers) = config.daemon().providers.as_table() else {
            return Ok(ConfigSources::default());
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
            if let Err(e) = lint_lines(
                &proto.path,
                proto.root.as_deref(),
                proto.data.as_deref(),
                proto.result.as_ref(),
                &env,
                &plugins,
            ) {
                errors.push(e);
            }
        }
        let built = crate::shape::build(
            providers,
            &self.registered_sources(),
            &engine_defaults(config),
        );
        let (decls, retunes) = match built {
            Ok(built) => (built.decls, built.retunes),
            Err(e) => {
                errors.extend(e);
                return Err(errors);
            }
        };
        let mut out = Vec::new();
        for decl in decls {
            let mut built = Vec::new();
            for spec in decl.sources {
                match ConfigSource::new(spec, env.clone(), plugins.clone(), self.reader.clone()) {
                    Ok(source) => built.push(source),
                    Err(e) => errors.push(e),
                }
            }
            out.push((decl.name, built));
        }
        if errors.is_empty() {
            Ok(ConfigSources {
                providers: out,
                retunes,
            })
        } else {
            Err(errors)
        }
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

    /// The Sources that answer `field` (dotted, below the provider): the Source
    /// declared at the nearest ancestor node of the key — `field` itself, then
    /// each shorter dotted prefix (`foo.bar.baz` tries `foo.bar.baz`, `foo.bar`,
    /// `foo`) — else every Source that declares a sentinel field (they answer for
    /// the provider node), in declaration order, else none.
    pub fn sources_for_key(&self, provider: &str, field: &str) -> Vec<&str> {
        let mut node = field;
        loop {
            if let Some(src) = self
                .declared_sources
                .get(&(provider.to_string(), node.to_string()))
            {
                return vec![src.as_str()];
            }
            match node.rfind('.') {
                Some(i) => node = &node[..i],
                None => break,
            }
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
        self.static_names.contains_key(name)
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

    /// Build a registry with all built-in providers registered.
    /// Panics on registration failure (programmer error, not runtime).
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        // TODO(phase1-section-H): re-enable each line once the provider is migrated
        // to the new Provider/Source trait shape. Uncomment one at a time during Section H.
        //
        registry
            .register(Box::new(crate::provider::asdf::AsdfProvider))
            .expect("asdf");
        registry
            .register(Box::new(crate::provider::aws::AwsProvider))
            .expect("aws_profiles");
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        registry
            .register(Box::new(crate::provider::battery::BatteryProvider))
            .expect("battery");
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
            .register(Box::new(crate::provider::hostname::HostnameProvider))
            .expect("hostname");
        registry
            .register(Box::new(crate::provider::load::LoadProvider))
            .expect("load");
        registry
            .register(Box::new(crate::provider::mise::MiseProvider))
            .expect("mise");
        registry
            .register(Box::new(crate::provider::network::NetworkProvider))
            .expect("network");
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
            .register(Box::new(crate::provider::uname::UnameProvider))
            .expect("uname");
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        registry
            .register(Box::new(crate::provider::uptime::UptimeProvider))
            .expect("uptime");
        registry
            .register(Box::new(crate::provider::user::UserProvider))
            .expect("user");
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

        // ── Config Sources, and the built-ins a return retunes ────────────────
        let mut errors = Vec::new();
        let builtins: Vec<(String, String)> = registry.sources.keys().cloned().collect();
        match registry.config_sources(config, vars) {
            Ok(ConfigSources { providers, retunes }) => {
                for (name, sources) in providers {
                    if let Err(e) = registry.register_config_sources(&name, sources) {
                        errors.push(e);
                    }
                }
                errors.extend(registry.retune_builtins(config, &builtins, &retunes));
            }
            Err(e) => errors.extend(e),
        }

        // ── Providers only the config names ───────────────────────────────────
        // A provider exists if either root names it. One named only in
        // `[client.providers]` or `[daemon.providers]` has no Source behind
        // it: it is registered with zero. `_`, the prototype's name, is never
        // one (`Config::named_providers`).
        for name in config.named_providers() {
            registry.register_config_named(&name);
        }

        errors.extend(config.errors());
        ConfigLoad { registry, errors }
    }

    /// Every built-in Source runs on `[daemon] budget.execute`; one whose
    /// `return` the files set takes its schedule, failback, decay and budget
    /// from that line instead. A built-in with no `return` keeps the rest of
    /// what it declares: the `_` lines apply to config Sources only.
    fn retune_builtins(
        &mut self,
        config: &Config,
        builtins: &[(String, String)],
        retunes: &[Retune],
    ) -> Vec<String> {
        let budget = config.daemon().budget_execute();
        let mut errors = Vec::new();
        for (provider, source) in builtins {
            let line = retunes
                .iter()
                .find(|r| &r.provider == provider && &r.source == source);
            self.retune(provider, source, |meta| {
                meta.budget_execute = budget;
                if let Some(Retune { recorded, .. }) = line {
                    meta.fsevents_reinstate =
                        !matches!(recorded.invalidation, InvalidationStrategy::Poll { .. });
                    meta.invalidation = recorded.invalidation.clone();
                    meta.failback = recorded.failback;
                    meta.decay = recorded.decay;
                    meta.budget_execute = recorded.budget_execute;
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
        let relative = match &meta.invalidation {
            InvalidationStrategy::Watch { patterns, .. }
            | InvalidationStrategy::WatchAndPoll { patterns, .. } => patterns.as_slice(),
            InvalidationStrategy::Poll { .. } => &[],
        };
        if !rooted && !relative.is_empty() {
            let listed: Vec<String> = relative.iter().map(|p| format!("'{p}'")).collect();
            return Err(format!(
                "{at} return: watch({}) is relative, but {provider}.{source} has one instance; \
                 use an absolute path",
                listed.join(", ")
            ));
        }
        self.providers[provider]
            .validate()
            .map_err(|e| format!("{at} return: {e}"))
    }
}

/// What a Source gets for a decorator its `return` does not name: every Source
/// has a `failback` and a `budget`, with no off switch.
fn engine_defaults(config: &Config) -> EngineDefaults {
    EngineDefaults {
        failback: FailbackConfig {
            reattempts: DEFAULT_FAILBACK_COUNT,
            interval_secs: DEFAULT_FAILBACK_INTERVAL_SECS,
        },
        budget_execute: config.daemon().budget_execute(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{
        FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, SourceResult,
    };

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
            fsevents_reinstate: false,
            decay: None,
            budget_execute: crate::scheduler::lifecycle::DEFAULT_BUDGET_EXECUTE,
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

    #[test]
    fn a_kept_root_is_re_resolved_once_its_instance_is_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let sub = base.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let sub_str = sub.to_str().unwrap();
        let reg = registry_from(
            "[daemon.providers.demo.sources.s]\nroot = \"{{ lup('.root') | dirname }}\"\ndata = \"{{ 1 }}\"\nresult.x = \"{{ data }}\"\n",
        );

        assert_eq!(reg.instance_root("demo", "s", sub_str), Ok(None));
        std::fs::write(sub.join(".root"), "").unwrap();
        assert_eq!(
            reg.instance_root("demo", "s", sub_str),
            Ok(None),
            "the answer is kept while its instance lives"
        );
        reg.forget_roots("demo", "s", Some("/elsewhere"));
        assert_eq!(
            reg.instance_root("demo", "s", sub_str),
            Ok(None),
            "another instance going cold says nothing about this one"
        );
        reg.forget_roots("demo", "s", None);
        assert_eq!(
            reg.instance_root("demo", "s", sub_str),
            Ok(Some(sub_str.to_string())),
            "once the instance it resolved to is gone, the root is looked up again"
        );
    }

    #[test]
    fn a_providers_sources_that_find_their_root_alike_share_one_lookup() {
        let reg = ProviderRegistry::with_defaults();
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
        assert_eq!(
            reg.config_source_count(),
            1,
            "demo.s takes its lines from `_`; the retuned built-in and `_` itself are not config Sources"
        );
        assert_eq!(registry_from("").config_source_count(), 0);
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
        let stock = ProviderRegistry::with_defaults();
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
        let stock = ProviderRegistry::with_defaults();
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
        // A return that names neither: the engine's failback, the daemon's budget.
        let reg = registry_from(
            "[daemon]\nbudget.execute = \"30ms\"\n\
             [daemon.providers.git.sources.diff]\nreturn = \"{{ result | poll('15s') }}\"\n",
        );
        let diff = source_meta(&reg, "git", "diff");
        assert_ne!(
            ProviderRegistry::with_defaults()
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
        assert_eq!(diff.budget_execute, std::time::Duration::from_millis(30));
        // Its own line wins when it names them.
        let reg = registry_from(
            "[daemon.providers.git.sources.diff]\n\
             return = \"{{ result | failback(5, '2s') | budget('7ms') }}\"\n",
        );
        let diff = source_meta(&reg, "git", "diff");
        assert_eq!(
            (diff.failback.reattempts, diff.failback.interval_secs),
            (5, 2)
        );
        assert_eq!(diff.budget_execute, std::time::Duration::from_millis(7));
        // And the budget reaches the built-ins that have no return.
        let reg = registry_from("[daemon]\nbudget.execute = \"30ms\"\n");
        assert_eq!(
            source_meta(&reg, "git", "refs").budget_execute,
            std::time::Duration::from_millis(30)
        );
    }

    #[test]
    fn a_builtins_super_reaches_the_prototypes_like_a_config_sources() {
        // own -> git's `_` -> the abstract `_` (the shipped poll('60s', 12)).
        let reg = registry_from(
            r#"
[daemon.providers.git.sources._]
return = "{{ result | failback(7, '3s') | super }}"

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
            "git's `_`, one level up, beats the abstract failback(3, '1s')"
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
        let registry = ProviderRegistry::with_defaults().share(&cache);
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
}
