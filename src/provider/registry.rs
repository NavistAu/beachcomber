use crate::provider::Provider;
use crate::provider::hostname::HostnameProvider;
use crate::provider::user::UserProvider;
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
