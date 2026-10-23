//! Seeding the config-seeded facets (spec §3.1, §3.2, §3.9).
//!
//! Two tables in configuration are data the daemon writes into the cache, and
//! nothing else is seeded from config:
//!
//! - `[client.providers.<p>] template` → namespace `<p>:template`, whose string
//!   leaves are value expressions the client renders;
//! - `[daemon.providers.<p>.cache]` → namespace `<p>:cache` (`*:cache` for the
//!   abstract provider `<p> = _`), whose only key is the `_.decay` prototype.
//!
//! What each file seeds, and what is wrong with it, comes from one per-file
//! parse (`crate::shape::seeded::read`). Each is written at the global path, every record labelled `config:<file>`
//! with the file that set it. Layers are written in composition order —
//! `defaults/*.toml` lexically, then the main config file, then
//! `conf.d/*.toml` lexically — with the deep-merge semantics
//! `Cache::write_static` gives a `put`. A record two files
//! set ends up labelled by the later one (a `config:` write moves a record out of
//! whichever earlier `config:` map held it), and a record only the earlier file set keeps
//! its own label.
//!
//! Settings still compose by deep merge. These tables deliberately do not: the
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

/// Seed the config-seeded tables of `config`'s files and watch them for changes.
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

/// Write every seeded table of every layer into the cache, one `write_static`
/// per (layer, namespace). Used at daemon start, where startup validation has
/// already refused any file with an error; what a file with an error would have
/// seeded is not written, only what validated. Refusals are logged.
pub fn seed_facets(cache: &Cache, registry: &ProviderRegistry, layers: &[(PathBuf, toml::Value)]) {
    let (planned, errors) = plan(layers);
    for e in &errors {
        tracing::error!("{e}");
    }
    for (namespace, writes) in planned {
        registry.register_static(&namespace);
        for (label, value) in writes {
            if let Err(e) = cache.write_static(&namespace, None, &label, "", value, None) {
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
/// exactly as it is and stays on top, because only `config:` labels are
/// dropped and the `put` layer reads above them.
/// A `watch` wakes either way — `Cache::reseed_static` notifies the namespace
/// whenever it drops or writes anything.
pub fn reseed_facets(
    cache: &Cache,
    registry: &ProviderRegistry,
    layers: &[(PathBuf, toml::Value)],
) {
    let (mut planned, errors) = plan(layers);
    if !errors.is_empty() {
        for e in &errors {
            tracing::error!("{}", e);
        }
        tracing::error!(
            "configuration seeded tables changed but are not valid ({} error(s)); \
             keeping the current seed",
            errors.len()
        );
        return;
    }

    // A namespace the layers no longer mention still holds the records the last
    // seed left there: visit it with an empty write list so the drop happens.
    for namespace in cache.global_namespaces_with_label_prefix(CONFIG_SOURCE_PREFIX) {
        planned.entry(namespace).or_default();
    }

    for (namespace, writes) in planned {
        registry.register_static(&namespace);
        if let Err(e) = cache.reseed_static(&namespace, None, CONFIG_SOURCE_PREFIX, writes) {
            tracing::warn!("could not re-seed {namespace}: {e}");
        }
    }
}

/// The write list per facet namespace, in composition order, and every file's
/// errors. Pure: registering the namespaces is the caller's, so the plan can be
/// built and inspected without touching the registry.
fn plan(layers: &[(PathBuf, toml::Value)]) -> (HashMap<String, Writes>, Vec<String>) {
    let mut out: HashMap<String, Writes> = HashMap::new();
    let mut errors = Vec::new();
    for (file, layer) in layers {
        let label = Config::config_label(file);
        let read = crate::shape::seeded::read(file, layer);
        errors.extend(read.errors);
        for (namespace, tree) in read.writes {
            let value = Value::from_json(&tree).expect("TOML has no null");
            out.entry(namespace)
                .or_default()
                .push((label.clone(), value));
        }
    }
    (out, errors)
}
