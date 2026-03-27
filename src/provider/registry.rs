use crate::provider::Provider;
use crate::provider::hostname::HostnameProvider;
use crate::provider::user::UserProvider;
use crate::provider::git::GitProvider;
use crate::provider::battery::BatteryProvider;
use crate::provider::load::LoadProvider;
use crate::provider::uptime::UptimeProvider;
use crate::provider::network::NetworkProvider;
use std::collections::HashMap;

#[derive(Default)]
pub struct ProviderRegistry {
    providers: HashMap<String, Box<dyn Provider>>,
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
        registry.register(Box::new(UptimeProvider));
        registry.register(Box::new(NetworkProvider));
        registry
    }

    pub fn register(&mut self, provider: Box<dyn Provider>) {
        let name = provider.metadata().name.clone();
        self.providers.insert(name, provider);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Provider> {
        self.providers.get(name).map(|p| p.as_ref())
    }

    pub fn list(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }
}
