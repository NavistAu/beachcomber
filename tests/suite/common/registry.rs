//! Registries for tests that build one in-process, with no daemon and so no
//! `defaults/` directory behind it. The shipped files come in as they do in
//! production: as the lowest layers, every one of them
//! (`daemon::defaults::shipped_layers`), composed under whatever the test adds.
//!
//! A registry with no shipped layers has no providers: a test that needs them
//! builds its registry here.

use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use std::path::PathBuf;

/// A configuration of every shipped layer, then `layers`.
pub fn config_over_shipped(layers: Vec<(PathBuf, toml::Value)>) -> Config {
    let mut all = beachcomber::daemon::defaults::shipped_layers();
    all.extend(layers);
    Config::from_layers(all)
}

/// The registry of the shipped defaults alone, as a daemon with no files of
/// its own started in this process loads it: its Sources' lines see this
/// process's environment (the instance at `~` runs in its `HOME`).
pub fn with_shipped_defaults() -> ProviderRegistry {
    ProviderRegistry::shipped(&std::env::vars().collect())
}
