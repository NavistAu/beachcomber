use crate::config::Config;
use crate::provider::Provider;
use crate::provider::ProviderSource;
use crate::provider::asdf::AsdfProvider;
use crate::provider::aws::AwsProvider;
use crate::provider::battery::BatteryProvider;
use crate::provider::conda::CondaProvider;
use crate::provider::direnv::DirenvProvider;
use crate::provider::gcloud::GcloudProvider;
use crate::provider::git::GitProvider;
use crate::provider::hostname::HostnameProvider;
use crate::provider::http::HttpProvider;
use crate::provider::kubecontext::KubecontextProvider;
use crate::provider::library::LibraryProvider;
use crate::provider::load::LoadProvider;
use crate::provider::mise::MiseProvider;
use crate::provider::network::NetworkProvider;
use crate::provider::op::OpProvider;
use crate::provider::python::PythonProvider;
use crate::provider::script::ScriptProvider;
use crate::provider::sudo::SudoProvider;
use crate::provider::terraform::TerraformProvider;
use crate::provider::uname::UnameProvider;
#[cfg(target_os = "macos")]
use crate::provider::uptime::UptimeProvider;
#[cfg(target_os = "linux")]
use crate::provider::uptime::UptimeProvider;
use crate::provider::user::UserProvider;
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default)]
pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn Provider>>,
    sources: HashMap<String, ProviderSource>,
    virtual_names: DashMap<String, ()>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
            sources: HashMap::new(),
            virtual_names: DashMap::new(),
        }
    }

    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        registry.register(Box::new(HostnameProvider));
        registry.register(Box::new(UserProvider));
        registry.register(Box::new(GitProvider));
        registry.register(Box::new(BatteryProvider));
        registry.register(Box::new(LoadProvider));
        registry.register(Box::new(UnameProvider));
        #[cfg(target_os = "macos")]
        registry.register(Box::new(UptimeProvider));
        #[cfg(target_os = "linux")]
        registry.register(Box::new(UptimeProvider));
        registry.register(Box::new(NetworkProvider));
        registry.register(Box::new(KubecontextProvider));
        registry.register(Box::new(AwsProvider));
        registry.register(Box::new(GcloudProvider));
        registry.register(Box::new(TerraformProvider));
        registry.register(Box::new(DirenvProvider));
        registry.register(Box::new(PythonProvider));
        registry.register(Box::new(CondaProvider));
        registry.register(Box::new(MiseProvider));
        registry.register(Box::new(AsdfProvider));
        registry.register(Box::new(SudoProvider));
        registry.register(Box::new(OpProvider));
        registry
    }

    pub fn with_config(config: &Config) -> Self {
        let mut registry = Self::new();

        // Register built-in providers unless disabled by config.
        let mut builtins: Vec<(&str, Box<dyn Provider>)> = vec![
            ("hostname", Box::new(HostnameProvider)),
            ("user", Box::new(UserProvider)),
            ("git", Box::new(GitProvider)),
            ("battery", Box::new(BatteryProvider)),
            ("load", Box::new(LoadProvider)),
            ("uname", Box::new(UnameProvider)),
            ("network", Box::new(NetworkProvider)),
            ("kubecontext", Box::new(KubecontextProvider)),
            ("aws", Box::new(AwsProvider)),
            ("gcloud", Box::new(GcloudProvider)),
            ("terraform", Box::new(TerraformProvider)),
            ("direnv", Box::new(DirenvProvider)),
            ("python", Box::new(PythonProvider)),
            ("conda", Box::new(CondaProvider)),
            ("mise", Box::new(MiseProvider)),
            ("asdf", Box::new(AsdfProvider)),
            ("sudo", Box::new(SudoProvider)),
            ("op", Box::new(OpProvider)),
        ];

        #[cfg(target_os = "macos")]
        builtins.push(("uptime", Box::new(UptimeProvider)));
        #[cfg(target_os = "linux")]
        builtins.push(("uptime", Box::new(UptimeProvider)));

        for (name, provider) in builtins {
            if !config.is_provider_disabled(name) {
                registry.register_with_source(provider, ProviderSource::Builtin);
            }
        }

        // Register script providers from config unless disabled.
        for (name, script_config) in config.script_providers() {
            if !config.is_provider_disabled(&name) {
                registry.register_with_source(
                    Box::new(ScriptProvider::new(&name, script_config)),
                    ProviderSource::Script,
                );
            }
        }

        // Register library providers from config unless disabled.
        for (name, lib_config) in config.library_providers() {
            if !config.is_provider_disabled(&name)
                && let Some(provider) = LibraryProvider::new(&name, lib_config)
            {
                registry.register_with_source(Box::new(provider), ProviderSource::Script);
            }
        }

        // Register HTTP providers from config unless disabled.
        for (name, http_config) in config.http_providers() {
            if !config.is_provider_disabled(&name) {
                registry.register_with_source(
                    Box::new(HttpProvider::new(&name, http_config)),
                    ProviderSource::Script,
                );
            }
        }

        registry
    }

    /// Register a provider with an explicit source.
    pub fn register_with_source(&mut self, provider: Box<dyn Provider>, source: ProviderSource) {
        let meta = provider.metadata();
        // Only validate builtin providers — script/library/HTTP providers may
        // legitimately omit field declarations and discover fields at runtime.
        if source == ProviderSource::Builtin {
            meta.validate()
                .unwrap_or_else(|e| panic!("invalid provider metadata: {}", e));
        }
        let name = meta.name.clone();
        self.sources.insert(name.clone(), source);
        self.providers.insert(name, Arc::from(provider));
    }

    /// Register a provider. Accepts a Box and converts internally to Arc.
    /// Convenience wrapper that uses `ProviderSource::Builtin`.
    pub fn register(&mut self, provider: Box<dyn Provider>) {
        self.register_with_source(provider, ProviderSource::Builtin);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Provider>> {
        self.providers.get(name).cloned()
    }

    pub fn get_source(&self, name: &str) -> Option<ProviderSource> {
        if let Some(source) = self.sources.get(name) {
            Some(*source)
        } else if self.virtual_names.contains_key(name) {
            Some(ProviderSource::Virtual)
        } else {
            None
        }
    }

    /// Returns true if the provider exists and its source is Builtin or Script
    /// (i.e., not Virtual).
    pub fn has_non_virtual(&self, name: &str) -> bool {
        matches!(
            self.sources.get(name),
            Some(ProviderSource::Builtin) | Some(ProviderSource::Script)
        )
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
}
