use crate::boundaries::library::LibloadingLoader;
use crate::cache::{Cache, Layers};
use crate::config::Config;
use crate::engine::PluginSet;
use crate::provider::config_source::{ConfigSource, DaemonRead, lint_lines};
use crate::provider::{Provider, ProviderMetadata, Source, SourceMetadata, Value, is_sentinel};
use dashmap::DashMap;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock, Weak};

/// What [`ProviderRegistry::load`] made of a configuration.
pub struct ConfigLoad {
    /// The providers that did load; trustworthy only when `errors` is empty.
    pub registry: ProviderRegistry,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
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
    /// global entry, as `QueryPlan` normalises it.
    fn read(&self, provider: &str, dir: Option<&str>) -> Option<HashMap<String, Value>> {
        let Some((registry, cache)) = self.bound.get() else {
            debug_assert!(false, "a config Source read from a registry never shared");
            return None;
        };
        let registry = registry.upgrade()?;
        let plan = crate::query::QueryPlan::build(provider, dir, &registry).ok()?;
        cache
            .read_merged(
                &plan.provider,
                plan.requested_path.as_deref(),
                plan.effective_path.as_deref(),
                &registry,
                Layers::All,
            )
            .map(|merged| merged.fields)
    }
}

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
    /// Providers with a config Source whose `root` line is set: resolving a
    /// request's path for one renders that line, which may run a command.
    config_roots: HashSet<String>,
    /// What every config Source reads `comb.<p>` through.
    reader: Arc<RegistryReader>,
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
            config_roots: HashSet::new(),
            reader: Arc::new(RegistryReader::default()),
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

    /// Whether resolving a request's path for `provider` may run a `root`
    /// line, and so must not hold an async worker.
    pub fn renders_roots(&self, provider: &str) -> bool {
        self.config_roots.contains(provider)
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
        if sources.iter().any(ConfigSource::has_root) {
            self.config_roots.insert(provider.to_string());
        }
        for source in sources {
            self.sources.insert(
                (provider.to_string(), source.metadata().name.clone()),
                Arc::new(source),
            );
        }
        self.providers.insert(provider.to_string(), meta);
        Ok(())
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
    ) -> Result<Vec<(String, Vec<ConfigSource>)>, Vec<String>> {
        let Some(providers) = config.daemon.providers.as_table() else {
            return Ok(Vec::new());
        };
        let search = config
            .config_path
            .as_deref()
            .and_then(std::path::Path::parent)
            .map(|dir| vec![dir.join("plugins")])
            .unwrap_or_default();
        let plugins = Arc::new(PluginSet::new(search, Arc::new(LibloadingLoader)));
        let env = Arc::new(env.clone());

        let mut errors = Vec::new();
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
        let decls = match crate::shape::build(providers, &self.registered_sources()) {
            Ok(decls) => decls,
            Err(e) => {
                errors.extend(e);
                return Err(errors);
            }
        };
        let mut out = Vec::new();
        for decl in decls {
            if config.is_provider_disabled(&decl.name) {
                continue;
            }
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
            Ok(out)
        } else {
            Err(errors)
        }
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

    /// Return the slice of SourceMetadata for all sources declared by a provider.
    pub fn provider_sources(&self, provider: &str) -> Option<&[SourceMetadata]> {
        self.providers.get(provider).map(|m| m.sources.as_slice())
    }

    /// Return the list of registered provider names (excluding static ones).
    pub fn provider_names(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }

    /// Every provider's declared source names, keyed by provider — the shape
    /// `Config::validate_providers` takes.
    pub fn source_names_by_provider(&self) -> HashMap<String, Vec<String>> {
        self.providers
            .iter()
            .map(|(name, meta)| {
                (
                    name.clone(),
                    meta.sources.iter().map(|s| s.name.clone()).collect(),
                )
            })
            .collect()
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

    /// [`Self::load`] with libraries: the registry, or every error the config has.
    pub fn with_config(
        config: &Config,
        vars: &HashMap<String, String>,
    ) -> Result<Self, Vec<String>> {
        let load = Self::load(config, vars, true);
        if load.errors.is_empty() {
            Ok(load.registry)
        } else {
            Err(load.errors)
        }
    }

    /// The one loader: startup, the config watch's gate and `comb check
    /// config` all build the provider set here, so they agree on what loads.
    /// Built-ins (minus the disabled), the HTTP providers (metadata only, no
    /// I/O), the config Sources (`[daemon.providers.<p>.sources.<s>]`, whose
    /// lines see `vars`, the daemon's own variables), and the providers only
    /// the config names; then every check a config has: the per-file ones, the
    /// old-form provider blocks, and the Sources against the set built.
    ///
    /// `libraries` loads library providers, which `dlopen` their file: the
    /// daemon does, a check or the watch must not. Their fields are therefore
    /// unknown to a check, and a config Source colliding with one is found at
    /// startup only. (The gap closes when the flat library form goes.)
    ///
    /// Panics on built-in registration failure (programmer error).
    pub fn load(config: &Config, vars: &HashMap<String, String>, libraries: bool) -> ConfigLoad {
        use crate::provider::http::HttpProvider;
        use crate::provider::library::LibraryProvider;

        // Start from the full built-in set, then layer on config-defined providers.
        // Built-ins that are explicitly disabled in config are not added.
        let mut registry = Self::new();

        macro_rules! add_builtin {
            ($name:expr, $provider:expr) => {
                if !config.is_provider_disabled($name) {
                    registry.register(Box::new($provider)).expect($name);
                }
            };
        }

        add_builtin!("asdf", crate::provider::asdf::AsdfProvider);
        add_builtin!("aws_profiles", crate::provider::aws::AwsProvider);
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        add_builtin!("battery", crate::provider::battery::BatteryProvider);
        add_builtin!("direnv", crate::provider::direnv::DirenvProvider);
        add_builtin!("gcloud_configs", crate::provider::gcloud::GcloudProvider);
        add_builtin!("git", crate::provider::git::GitProvider);
        add_builtin!("hostname", crate::provider::hostname::HostnameProvider);
        add_builtin!("load", crate::provider::load::LoadProvider);
        add_builtin!("mise", crate::provider::mise::MiseProvider);
        add_builtin!("network", crate::provider::network::NetworkProvider);
        add_builtin!("python", crate::provider::python::PythonProvider);
        add_builtin!("sudo", crate::provider::sudo::SudoProvider);
        add_builtin!("terraform", crate::provider::terraform::TerraformProvider);
        add_builtin!("uname", crate::provider::uname::UnameProvider);
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        add_builtin!("uptime", crate::provider::uptime::UptimeProvider);
        add_builtin!("user", crate::provider::user::UserProvider);

        // Registration failures are the user's to see: `comb check config` shows them.
        let mut warnings: Vec<String> = Vec::new();
        // ── Single-source external backends (`type = "library" / "http"`) ──
        for (name, lib_config) in config.library_providers() {
            if libraries
                && !config.is_provider_disabled(&name)
                && let Some(provider) = LibraryProvider::new(&name, lib_config)
                && let Err(e) = registry.register(Box::new(provider))
            {
                warnings.push(format!("Failed to register library provider '{name}': {e}"));
            }
        }
        for (name, http_config) in config.http_providers() {
            if !config.is_provider_disabled(&name)
                && let Err(e) = registry.register(Box::new(HttpProvider::new(&name, http_config)))
            {
                warnings.push(format!("Failed to register http provider '{name}': {e}"));
            }
        }

        // ── Phase 4 multi-source external backends ────────────────────────────
        // `backend = "library" / "http"` TOML shape.

        // Library
        match config.multi_library_providers() {
            Ok(providers) => {
                for (name, lib_path, source_overrides) in providers {
                    if libraries
                        && !config.is_provider_disabled(&name)
                        && let Some(provider) =
                            LibraryProvider::with_sources(&name, &lib_path, source_overrides)
                        && let Err(e) = registry.register(Box::new(provider))
                    {
                        warnings.push(format!(
                            "Failed to register multi-source library provider '{name}': {e}"
                        ));
                    }
                }
            }
            Err(e) => warnings.push(format!("Error reading multi-source library providers: {e}")),
        }

        // HTTP
        match config.multi_http_providers() {
            Ok(providers) => {
                for (name, _default_timeout, sources) in providers {
                    if !config.is_provider_disabled(&name) {
                        if sources.is_empty() {
                            warnings.push(format!(
                                "HTTP provider '{name}' (backend = http) has no source blocks; skipping"
                            ));
                            continue;
                        }
                        if let Err(e) =
                            registry.register(Box::new(HttpProvider::with_sources(&name, sources)))
                        {
                            warnings.push(format!(
                                "Failed to register multi-source HTTP provider '{name}': {e}"
                            ));
                        }
                    }
                }
            }
            Err(e) => warnings.push(format!("Error reading multi-source HTTP providers: {e}")),
        }

        // ── Config Sources ────────────────────────────────────────────────────
        let mut errors = Vec::new();
        match registry.config_sources(config, vars) {
            Ok(providers) => {
                for (name, sources) in providers {
                    for source in &sources {
                        let source = &source.metadata().name;
                        // A knob block is a second way to set what `return` sets.
                        if config
                            .providers
                            .get(&name)
                            .and_then(|p| p.get(source))
                            .is_some_and(toml::Value::is_table)
                        {
                            errors.push(format!(
                                "[providers.{name}.{source}] sets knobs on a config Source: poll, \
                                 watch and failback belong in the return line of \
                                 [daemon.providers.{name}.sources.{source}]"
                            ));
                        }
                    }
                    if let Err(e) = registry.register_config_sources(&name, sources) {
                        errors.push(e);
                    }
                }
            }
            Err(e) => errors.extend(e),
        }

        // ── Providers only the config names ───────────────────────────────────
        // A provider exists if either root names it. One named only in
        // `[client.providers]` or `[daemon.providers]` has no Source behind
        // it: it is registered with zero. `_`, the prototype's name, is never
        // one (`Config::named_providers`). A provider the config disables is
        // not brought back by a template naming it: until a provider's
        // settings and its Sources are one table, registering it with zero
        // Sources would leave `[providers.<p>.<source>]` matching nothing.
        for name in config.named_providers() {
            if !config.is_provider_disabled(&name) {
                registry.register_config_named(&name);
            }
        }

        let (provider_warnings, provider_errors) = config.validate_providers(
            &registry.provider_names(),
            &registry.source_names_by_provider(),
        );
        warnings.extend(provider_warnings);
        errors.extend(provider_errors);
        errors.extend(config.validate_seeded());
        ConfigLoad {
            registry,
            warnings,
            errors,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{
        FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, SourceResult, SourceScope,
    };

    struct FakeSource(SourceMetadata);
    impl Source for FakeSource {
        fn metadata(&self) -> &SourceMetadata {
            &self.0
        }
        fn execute(&self, _path: Option<&str>) -> SourceResult {
            SourceResult::new()
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
            scope: SourceScope::Global,
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
        assert_eq!(meta.scope, SourceScope::PathScoped);
        assert_eq!(
            reg.sources_for_key("demo", "anything"),
            vec!["s"],
            "a whole-node Source"
        );
    }

    #[test]
    fn a_disabled_provider_registers_no_config_sources() {
        let reg = registry_from(
            "[providers.demo]\nenabled = false\n[daemon.providers.demo.sources.s]\ndata = \"{{ 1 }}\"\nresult = \"{{ data }}\"\n",
        );
        assert!(reg.source("demo", "s").is_none());
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
