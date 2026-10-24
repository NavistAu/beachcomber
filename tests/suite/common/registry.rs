//! Registries for tests that build one in-process, with no daemon and so no
//! `defaults/` directory behind it. The abstract provider (`template`,
//! `result`, `return`) comes in as it does in production: as a layer, the
//! shipped `00-comb.toml`, composed under whatever the test adds.

use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use std::path::PathBuf;

/// A configuration of the shipped defaults layer, then `layers`.
pub fn config_over_shipped(layers: Vec<(PathBuf, toml::Value)>) -> Config {
    let mut all = vec![beachcomber::daemon::defaults::comb_layer()];
    all.extend(layers);
    Config::from_layers(all)
}

/// The built-in providers with the shipped defaults layer composed in.
pub fn with_shipped_defaults() -> ProviderRegistry {
    let load = ProviderRegistry::load(&config_over_shipped(Vec::new()), &Default::default());
    assert!(load.errors.is_empty(), "{:?}", load.errors);
    load.registry
}
