use crate::config::Config;
use crate::provider::{Provider, ProviderMetadata, Source, SourceMetadata, is_sentinel};
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;

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
        }
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
        // Populate the declared-field map and the sentinel list.
        for sm in &meta.sources {
            for f in &sm.fields {
                if is_sentinel(&f.name) {
                    let list = self.sentinel_sources.entry(meta.name.clone()).or_default();
                    if !list.contains(&sm.name) {
                        list.push(sm.name.clone());
                    }
                } else {
                    self.declared_sources
                        .insert((meta.name.clone(), f.name.clone()), sm.name.clone());
                }
            }
        }
        // Insert sources.
        for s in sources {
            let sn = s.metadata().name.clone();
            self.sources.insert((meta.name.clone(), sn), Arc::from(s));
        }
        self.providers.insert(meta.name.clone(), meta);
        Ok(())
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
    /// (builtin or script) provider.
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

    /// Build a registry with all built-in providers registered, applying config-level
    /// disable flags. Also registers script/library/HTTP providers from config.
    ///
    /// Panics on registration failure (programmer error, not runtime).
    pub fn with_config(config: &Config) -> Self {
        use crate::provider::http::HttpProvider;
        use crate::provider::library::LibraryProvider;
        use crate::provider::script::ScriptProvider;

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

        // ── Legacy single-source external backends ────────────────────────────
        // Old `type = "script" / "library" / "http"` TOML shape (no `backend` key).
        for (name, script_config) in config.script_providers() {
            if !config.is_provider_disabled(&name) {
                registry
                    .register(Box::new(ScriptProvider::new(&name, script_config)))
                    .unwrap_or_else(|e| {
                        tracing::warn!("Failed to register script provider '{}': {}", name, e);
                    });
            }
        }
        for (name, lib_config) in config.library_providers() {
            if !config.is_provider_disabled(&name)
                && let Some(provider) = LibraryProvider::new(&name, lib_config)
            {
                registry.register(Box::new(provider)).unwrap_or_else(|e| {
                    tracing::warn!("Failed to register library provider '{}': {}", name, e);
                });
            }
        }
        for (name, http_config) in config.http_providers() {
            if !config.is_provider_disabled(&name) {
                registry
                    .register(Box::new(HttpProvider::new(&name, http_config)))
                    .unwrap_or_else(|e| {
                        tracing::warn!("Failed to register http provider '{}': {}", name, e);
                    });
            }
        }

        // ── Phase 4 multi-source external backends ────────────────────────────
        // `backend = "script" / "library" / "http"` TOML shape.

        // Script
        match config.multi_script_providers() {
            Ok(providers) => {
                for (name, sources) in providers {
                    if !config.is_provider_disabled(&name) {
                        if sources.is_empty() {
                            tracing::warn!(
                                "Script provider '{}' (backend = script) has no source blocks; skipping",
                                name
                            );
                            continue;
                        }
                        registry
                            .register(Box::new(ScriptProvider::with_sources(&name, sources)))
                            .unwrap_or_else(|e| {
                                tracing::warn!(
                                    "Failed to register multi-source script provider '{}': {}",
                                    name,
                                    e
                                );
                            });
                    }
                }
            }
            Err(e) => tracing::warn!("Error reading multi-source script providers: {}", e),
        }

        // Library
        match config.multi_library_providers() {
            Ok(providers) => {
                for (name, lib_path, source_overrides) in providers {
                    if !config.is_provider_disabled(&name)
                        && let Some(provider) =
                            LibraryProvider::with_sources(&name, &lib_path, source_overrides)
                    {
                        registry.register(Box::new(provider)).unwrap_or_else(|e| {
                            tracing::warn!(
                                "Failed to register multi-source library provider '{}': {}",
                                name,
                                e
                            );
                        });
                    }
                }
            }
            Err(e) => tracing::warn!("Error reading multi-source library providers: {}", e),
        }

        // HTTP
        match config.multi_http_providers() {
            Ok(providers) => {
                for (name, _default_timeout, sources) in providers {
                    if !config.is_provider_disabled(&name) {
                        if sources.is_empty() {
                            tracing::warn!(
                                "HTTP provider '{}' (backend = http) has no source blocks; skipping",
                                name
                            );
                            continue;
                        }
                        registry
                            .register(Box::new(HttpProvider::with_sources(&name, sources)))
                            .unwrap_or_else(|e| {
                                tracing::warn!(
                                    "Failed to register multi-source HTTP provider '{}': {}",
                                    name,
                                    e
                                );
                            });
                    }
                }
            }
            Err(e) => tracing::warn!("Error reading multi-source HTTP providers: {}", e),
        }

        registry
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
