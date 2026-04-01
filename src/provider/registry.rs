use crate::config::Config;
use crate::provider::Provider;
use crate::provider::hostname::HostnameProvider;
use crate::provider::user::UserProvider;
use crate::provider::git::GitProvider;
use crate::provider::battery::BatteryProvider;
use crate::provider::load::LoadProvider;
#[cfg(target_os = "macos")]
use crate::provider::uptime::UptimeProvider;
use crate::provider::network::NetworkProvider;
use crate::provider::kubecontext::KubecontextProvider;
use crate::provider::aws::AwsProvider;
use crate::provider::gcloud::GcloudProvider;
use crate::provider::terraform::TerraformProvider;
use crate::provider::direnv::DirenvProvider;
use crate::provider::python::PythonProvider;
use crate::provider::conda::CondaProvider;
use crate::provider::mise::MiseProvider;
use crate::provider::asdf::AsdfProvider;
use crate::provider::script::ScriptProvider;
use crate::provider::http::HttpProvider;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default)]
pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn Provider>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
        }
    }

    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        registry.register(Box::new(HostnameProvider));
        registry.register(Box::new(UserProvider));
        registry.register(Box::new(GitProvider));
        registry.register(Box::new(BatteryProvider));
        registry.register(Box::new(LoadProvider));
        #[cfg(target_os = "macos")]
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
        ];

        #[cfg(target_os = "macos")]
        builtins.push(("uptime", Box::new(UptimeProvider)));

        for (name, provider) in builtins {
            if !config.is_provider_disabled(name) {
                registry.register(provider);
            }
        }

        // Register script providers from config unless disabled.
        for (name, script_config) in config.script_providers() {
            if !config.is_provider_disabled(&name) {
                registry.register(Box::new(ScriptProvider::new(&name, script_config)));
            }
        }

        // Register HTTP providers from config unless disabled.
        for (name, http_config) in config.http_providers() {
            if !config.is_provider_disabled(&name) {
                registry.register(Box::new(HttpProvider::new(&name, http_config)));
            }
        }

        registry
    }

    /// Register a provider. Accepts a Box and converts internally to Arc.
    pub fn register(&mut self, provider: Box<dyn Provider>) {
        let name = provider.metadata().name.clone();
        self.providers.insert(name, Arc::from(provider));
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Provider>> {
        self.providers.get(name).cloned()
    }

    pub fn list(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }
}
