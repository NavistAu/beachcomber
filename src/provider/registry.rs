use crate::config::Config;
use crate::provider::{Provider, ProviderMetadata, Source, SourceMetadata};
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;

pub struct ProviderRegistry {
    /// (provider_name, source_name) → Arc-wrapped Source trait object.
    sources: HashMap<(String, String), Arc<dyn Source>>,
    /// Provider metadata snapshot, indexed by provider name.
    providers: HashMap<String, ProviderMetadata>,
    /// (provider_name, field_name) → source_name. Built once at registration.
    field_to_source: HashMap<(String, String), String>,
    /// Virtual provider names registered via `comb put`. Orthogonal to the source model.
    virtual_names: DashMap<String, ()>,
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
            field_to_source: HashMap::new(),
            virtual_names: DashMap::new(),
        }
    }

    /// Register a provider. Validates metadata, builds the field→source reverse map,
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
        // Populate field_to_source map.
        for sm in &meta.sources {
            for f in &sm.fields {
                self.field_to_source
                    .insert((meta.name.clone(), f.name.clone()), sm.name.clone());
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

    /// Given a field name on a provider, return the name of the source that owns it.
    pub fn source_for_field(&self, provider: &str, field: &str) -> Option<&str> {
        self.field_to_source
            .get(&(provider.to_string(), field.to_string()))
            .map(|s| s.as_str())
    }

    /// Return the slice of SourceMetadata for all sources declared by a provider.
    pub fn provider_sources(&self, provider: &str) -> Option<&[SourceMetadata]> {
        self.providers.get(provider).map(|m| m.sources.as_slice())
    }

    /// Return the list of registered provider names (excluding virtuals).
    pub fn provider_names(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }

    // ── Virtual provider support ─────────────────────────────────────────────

    /// Returns true if the provider name belongs to a real (non-virtual) provider.
    pub fn has_non_virtual(&self, name: &str) -> bool {
        self.providers.contains_key(name)
    }

    /// Returns true if the provider name was registered as virtual.
    pub fn is_virtual(&self, name: &str) -> bool {
        self.virtual_names.contains_key(name)
    }

    /// Register a virtual provider name. Returns false if a non-virtual provider
    /// already exists with this name.
    pub fn register_virtual(&self, name: &str) -> bool {
        if self.has_non_virtual(name) {
            return false;
        }
        self.virtual_names.insert(name.to_string(), ());
        true
    }

    /// Return all provider names, including virtual names.
    pub fn list(&self) -> Vec<String> {
        let mut names: Vec<String> = self.providers.keys().cloned().collect();
        for entry in self.virtual_names.iter() {
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
            .expect("aws");
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        registry
            .register(Box::new(crate::provider::battery::BatteryProvider))
            .expect("battery");
        registry
            .register(Box::new(crate::provider::conda::CondaProvider))
            .expect("conda");
        registry
            .register(Box::new(crate::provider::direnv::DirenvProvider))
            .expect("direnv");
        registry
            .register(Box::new(crate::provider::gcloud::GcloudProvider))
            .expect("gcloud");
        registry
            .register(Box::new(crate::provider::git::GitProvider))
            .expect("git");
        registry
            .register(Box::new(crate::provider::hostname::HostnameProvider))
            .expect("hostname");
        registry
            .register(Box::new(crate::provider::kubecontext::KubecontextProvider))
            .expect("kubecontext");
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
            .register(Box::new(crate::provider::op::OpProvider))
            .expect("op");
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
        add_builtin!("aws", crate::provider::aws::AwsProvider);
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        add_builtin!("battery", crate::provider::battery::BatteryProvider);
        add_builtin!("conda", crate::provider::conda::CondaProvider);
        add_builtin!("direnv", crate::provider::direnv::DirenvProvider);
        add_builtin!("gcloud", crate::provider::gcloud::GcloudProvider);
        add_builtin!("git", crate::provider::git::GitProvider);
        add_builtin!("hostname", crate::provider::hostname::HostnameProvider);
        add_builtin!(
            "kubecontext",
            crate::provider::kubecontext::KubecontextProvider
        );
        add_builtin!("load", crate::provider::load::LoadProvider);
        add_builtin!("mise", crate::provider::mise::MiseProvider);
        add_builtin!("network", crate::provider::network::NetworkProvider);
        add_builtin!("op", crate::provider::op::OpProvider);
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
        FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, SourceResult,
        SourceScope,
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
            invalidation: InvalidationStrategy::Poll { interval_secs: 30 },
            keep_alive: KeepAlive::Polls(2),
            failback: FailbackConfig {
                reattempts: 3,
                interval_secs: 30,
            },
            fsevents_reinstate: false,
        }
    }

    #[test]
    fn register_builds_field_to_source_map() {
        let mut reg = ProviderRegistry::new();
        let p = FakeProvider {
            name: "git".into(),
            sources: vec![ms("refs", vec!["branch"]), ms("diff", vec!["lines_added"])],
        };
        reg.register(Box::new(p)).unwrap();
        assert_eq!(reg.source_for_field("git", "branch"), Some("refs"));
        assert_eq!(reg.source_for_field("git", "lines_added"), Some("diff"));
        assert_eq!(reg.source_for_field("git", "nonexistent"), None);
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
    fn virtual_registration_blocked_by_real_provider() {
        let mut reg = ProviderRegistry::new();
        let p = FakeProvider {
            name: "hostname".into(),
            sources: vec![ms("name", vec!["value"])],
        };
        reg.register(Box::new(p)).unwrap();
        assert!(!reg.register_virtual("hostname"));
        assert!(reg.register_virtual("myvar"));
        assert!(reg.is_virtual("myvar"));
    }
}
