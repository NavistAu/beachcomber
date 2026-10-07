//! Seeding facet namespaces from configuration (spec §3.1, §3.2, §3.9).
//!
//! Every `[providers."<p>:<facet>"]` table in configuration is written into the
//! cache at namespace `<p>:<facet>` and the global path, each record labelled
//! `config:<file>` with the file that set it. Layers are written in composition
//! order — `defaults/*.toml` lexically, then the main config file, then
//! `conf.d/*.toml` lexically — with the deep-merge semantics
//! `Cache::write_static` gives a `put`. A record two files
//! set ends up labelled by the later one (a static write moves a record out of
//! whichever static map held it), and a record only the earlier file set keeps
//! its own label.
//!
//! Settings still compose by deep merge. Facet tables deliberately do not: the
//! merge would collapse the layers into one value and the file a record came
//! from — the thing `comb status` shows as its source — would be gone before it
//! reached the cache.

use crate::cache::{CONFIG_SOURCE_PREFIX, Cache};
use crate::config::Config;
use crate::provider::Value;
use crate::provider::registry::ProviderRegistry;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// One namespace's writes: the `(label, tree)` pairs to apply in composition
/// order.
type Writes = Vec<(String, Value)>;

/// Seed the facet tables of `config`'s files and watch them for changes.
///
/// Both halves are gated on `Config::config_path`, which only a
/// `Config::load()` sets: a daemon handed a `Config` built in memory (an
/// embedder, every test that does not want a config file) corresponds to no
/// file, so it reads and watches nothing rather than picking up the ambient
/// `~/.config/beachcomber`. The layers are re-read from that path rather than
/// taken from `config`, because composition deep-merges them into one value and
/// the per-file provenance every seeded record carries would be gone.
///
/// `polling_watch` mirrors the scheduler's file-watch seam: an in-process
/// daemon takes the mtime backstop on a short interval, because registering a
/// kernel fs-event watch costs about 1.2s per call on macOS and a test that is
/// not about the native backend should not pay it.
pub fn spawn_config_seeding(
    cache: &Arc<Cache>,
    registry: &Arc<ProviderRegistry>,
    config: &Config,
    cancel: &CancellationToken,
    polling_watch: Option<Duration>,
) {
    let Some(config_path) = config.config_path.clone() else {
        return;
    };
    let defaults_dir = Config::defaults_dir_for(&config_path);
    let conf_d_dir = Config::conf_d_dir_for(&config_path);
    seed_facets(
        cache,
        registry,
        &Config::layers(Some(&defaults_dir), Some(&config_path), &conf_d_dir),
    );

    // The watch stops on the daemon's own shutdown token — its closure holds
    // the cache and the registry, so a watch outliving the daemon would keep
    // both alive for the life of the process. A `Settings` change cancels that
    // same token, which is also what ends the watch.
    let stop = cancel.clone();
    let restart = stop.clone();
    let cache = cache.clone();
    let registry = registry.clone();
    let watched_path = config_path.clone();
    let (interval, fs_events) = match polling_watch {
        None => (super::config_watch::CONFIG_WATCH_POLL_INTERVAL, true),
        Some(_) => (Duration::from_millis(50), false),
    };
    super::config_watch::spawn_config_self_watch_with(
        config_path,
        interval,
        fs_events,
        stop,
        move |kind| match kind {
            super::config_watch::ChangeKind::FacetsOnly => {
                let layers = Config::layers(Some(&defaults_dir), Some(&watched_path), &conf_d_dir);
                reseed_facets(&cache, &registry, &layers);
            }
            super::config_watch::ChangeKind::Settings => {
                tracing::info!("configuration settings changed; shutting down for restart");
                restart.cancel();
            }
        },
    );
}

/// Write every facet table of every layer into the cache, one `write_static`
/// per (layer, namespace). Used at daemon start, where `validate_providers` has
/// already passed over the composed configuration. Refusals are logged.
pub fn seed_facets(cache: &Cache, registry: &ProviderRegistry, layers: &[(PathBuf, toml::Value)]) {
    for (namespace, writes) in plan(layers) {
        registry.register_static(&namespace);
        for (label, value) in writes {
            if let Err(e) =
                cache.write_static(&namespace, None, &label, "", value, false, None, registry)
            {
                tracing::warn!("{label}: could not seed {namespace}: {e}");
            }
        }
    }
}

/// Re-seed in place, with no restart. The new layers are validated first: on
/// any error nothing is touched and the previous seed keeps being served, the
/// errors logged (`comb check config` reports the same ones from the client
/// side). Otherwise each facet namespace is swapped under one write guard — its
/// `config:` records dropped and the new ones applied together, so no
/// concurrent `get` can see the gap between them.
///
/// The consequences, in the order an operator meets them: a leaf edited in a
/// file takes effect on the next read; a leaf deleted from a file disappears; a
/// leaf a later file overrides stays overridden; and a `put` record is left
/// exactly as it is, because only `config:` labels are dropped and
/// `Cache::reseed_static` drops any planned leaf the `put` map still occupies.
/// A `watch` wakes either way — `Cache::reseed_static` notifies the namespace
/// whenever it drops or writes anything.
pub fn reseed_facets(
    cache: &Cache,
    registry: &ProviderRegistry,
    layers: &[(PathBuf, toml::Value)],
) {
    let (warnings, errors) = Config::validate_facet_layers(layers);
    for w in &warnings {
        tracing::warn!("{}", w);
    }
    if !errors.is_empty() {
        for e in &errors {
            tracing::error!("{}", e);
        }
        tracing::error!(
            "configuration facet tables changed but are not valid ({} error(s)); \
             keeping the current seed",
            errors.len()
        );
        return;
    }

    let mut planned = plan(layers);
    // A namespace the layers no longer mention still holds the records the last
    // seed left there: visit it with an empty write list so the drop happens.
    for namespace in cache.global_namespaces_with_label_prefix(CONFIG_SOURCE_PREFIX) {
        planned.entry(namespace).or_default();
    }

    for (namespace, writes) in planned {
        registry.register_static(&namespace);
        if let Err(e) =
            cache.reseed_static(&namespace, None, CONFIG_SOURCE_PREFIX, writes, registry)
        {
            tracing::warn!("could not re-seed {namespace}: {e}");
        }
    }
}

/// The write list per facet namespace, in composition order. Pure: registering
/// the namespaces is the caller's, so the plan can be built and inspected
/// without touching the registry.
fn plan(layers: &[(PathBuf, toml::Value)]) -> HashMap<String, Writes> {
    let mut out: HashMap<String, Writes> = HashMap::new();
    for (file, layer) in layers {
        let label = Config::config_label(file);
        for (namespace, table) in Config::facet_tables(layer) {
            let is_template =
                crate::query::parse_facet(namespace).ok().flatten() == Some("template");
            let value =
                Value::from_json(&facet_json(table, is_template)).expect("TOML has no null");
            out.entry(namespace.to_string())
                .or_default()
                .push((label.clone(), value));
        }
    }
    out
}

/// One facet table as the JSON tree the cache stores: the TOML maps 1:1, with
/// nested tables becoming objects and a datetime as the text it was written as
/// (serde would render it as a private tagged object nobody can address).
///
/// Under `template` a leaf that is neither a string nor a table is not a value
/// expression, so it is dropped — validation has already warned about it by
/// name.
fn facet_json(table: &toml::Table, is_template: bool) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for (key, value) in table {
        let converted = match value {
            toml::Value::Table(sub) => facet_json(sub, is_template),
            toml::Value::String(s) => serde_json::Value::String(s.clone()),
            _ if is_template => continue,
            toml::Value::Datetime(d) => serde_json::Value::String(d.to_string()),
            other => serde_json::to_value(other).unwrap_or(serde_json::Value::Null),
        };
        out.insert(key.clone(), converted);
    }
    serde_json::Value::Object(out)
}
