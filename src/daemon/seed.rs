//! Seeding the config-seeded facets (spec §3.1, §3.2, §3.9).
//!
//! Two things in configuration are data the daemon writes into the cache, and
//! nothing else is seeded from config:
//!
//! - a provider's `template` line (`[daemon.providers.<p>] template`, by
//!   default `[client.providers.<p>] template` of the layer) → namespace
//!   `<p>:template`, whose string leaves are value expressions the client
//!   renders. The line is rendered here, once per layer, over that layer's
//!   `[client]` tree; the leaves themselves are never evaluated;
//! - `[daemon.providers.<p>.cache]` → namespace `<p>:cache` (`*:cache` for the
//!   abstract provider `<p> = _`), whose only key is the `_.decay` prototype.
//!
//! What the `cache` tables seed, and what is wrong with any file, comes from
//! one per-file parse (`crate::shape::seeded::read`). Each is written at the global path, every record labelled `config:<file>`
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
use crate::provider::registry::{ProviderRegistry, RegistryHandle};
use crate::scheduler::{SchedulerHandle, SchedulerMessage};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// One namespace's writes: the `(label, tree)` pairs to apply in composition
/// order.
type Writes = Vec<(String, Value)>;

/// Seed the config-seeded tables of `config`'s files and watch them for changes:
/// an edit under `providers` reloads the registry and reseeds in place
/// ([`reload_providers`]), a bare `[daemon]` or `[client]` key cancels `cancel`.
///
/// Both halves are gated on `Config::config_path`, which only
/// `Config::load_from` sets: a daemon handed a `Config` built in memory (an
/// embedder, every test that does not want a config file) corresponds to no
/// file, so it reads and watches nothing rather than picking up the ambient
/// `~/.config/beachcomber`. The first seed is of the layers `config` was
/// composed from, which are the ones startup validated; a change on disk is
/// read afresh, validated, and re-seeded.
///
/// `polling_watch` mirrors the scheduler's file-watch seam: an in-process
/// daemon takes the mtime backstop on a short interval, because registering a
/// kernel fs-event watch costs about 1.2s per call on macOS and a test that is
/// not about the native backend should not pay it.
pub fn spawn_config_seeding(
    cache: &Arc<Cache>,
    registry: &RegistryHandle,
    config: &Config,
    cancel: &CancellationToken,
    polling_watch: Option<Duration>,
    scheduler: &SchedulerHandle,
) {
    let Some(config_path) = config.config_file().map(|p| p.to_path_buf()) else {
        return;
    };
    seed_facets(cache, &registry.current(), config.composed_layers());

    // The watch stops on the daemon's own shutdown token — its closure holds
    // the cache and the registry, so a watch outliving the daemon would keep
    // both alive for the life of the process. A `Settings` change cancels that
    // same token, which is also what ends the watch.
    let stop = cancel.clone();
    let restart = stop.clone();
    let cache = cache.clone();
    let registry = registry.clone();
    let scheduler = scheduler.clone();
    // The last registry sent to the scheduler, which the registry handle lags
    // until the scheduler has handled the message: the next edit adopts from
    // this one, so two edits in quick succession do not lose what the first
    // carried.
    let mut last = registry.current();
    let (interval, fs_events) = match polling_watch {
        None => (super::config_watch::CONFIG_WATCH_POLL_INTERVAL, true),
        Some(_) => (Duration::from_millis(50), false),
    };
    super::config_watch::spawn_config_self_watch_with(
        config_path,
        interval,
        fs_events,
        stop,
        move |kind, changed, loaded| match kind {
            // What was compared and reloaded is what the gate validated.
            super::config_watch::ChangeKind::Providers => {
                reload_providers(&cache, &registry, &scheduler, &mut last, changed, loaded);
            }
            super::config_watch::ChangeKind::Settings => {
                tracing::info!("configuration settings changed; shutting down for restart");
                restart.cancel();
            }
        },
    );
}

/// Apply a change under `[daemon.providers]` or `[client.providers]` in place.
/// `loaded` is the registry the gate built from `changed`; it is made the one
/// to serve from ([`ProviderRegistry::adopt`]) and the seed is planned for it,
/// and only when both are sound does anything change: on any error the whole
/// reload is abandoned, the errors logged, the running registry and seed kept.
/// Then the template and `cache` facets are reseeded and the scheduler is told
/// to put the registry in force and bring its lifecycle entries to match.
/// Requests and scheduler runs that took the previous registry finish on it.
/// `last` is the registry the previous reload sent, or the one in force before
/// any (the handle lags the scheduler); it becomes `next` once sent.
/// The reseed comes before the scheduler's message so that when it compares
/// each entry's schedule with the new configuration the decay tables it
/// resolves from are the new ones.
pub fn reload_providers(
    cache: &Arc<Cache>,
    registry: &RegistryHandle,
    scheduler: &SchedulerHandle,
    last: &mut Arc<ProviderRegistry>,
    changed: &Config,
    loaded: ProviderRegistry,
) {
    let next = loaded.adopt(last, cache);
    let (planned, errors) = plan(changed.composed_layers(), &next);
    if !errors.is_empty() {
        for e in &errors {
            tracing::error!("{e}");
        }
        tracing::error!(
            "configuration changed but cannot be seeded ({} error(s)); keeping the current \
             providers and seed",
            errors.len()
        );
        return;
    }
    apply_reseed(cache, &next, planned);
    scheduler.send_blocking(SchedulerMessage::Reload {
        next: Arc::clone(&next),
        registry: registry.clone(),
    });
    *last = next;
}

/// Write every seeded table of every layer into the cache, one `write_static`
/// per (layer, namespace). Used at daemon start, where startup validation has
/// already refused any file with an error; what a file with an error would have
/// seeded is not written, only what validated. Refusals are logged.
pub fn seed_facets(cache: &Cache, registry: &ProviderRegistry, layers: &[(PathBuf, toml::Value)]) {
    let (planned, errors) = plan(layers, registry);
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

/// Every error planning the seed of `config`'s layers over `registry` has.
pub fn plan_errors(config: &Config, registry: &ProviderRegistry) -> Vec<String> {
    plan(config.composed_layers(), registry).1
}

/// Swap in a plan that [`plan`] made without errors.
fn apply_reseed(cache: &Cache, registry: &ProviderRegistry, mut planned: HashMap<String, Writes>) {
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
/// built and inspected without touching the registry (which only names the
/// providers there are).
fn plan(
    layers: &[(PathBuf, toml::Value)],
    registry: &ProviderRegistry,
) -> (HashMap<String, Writes>, Vec<String>) {
    let mut out: HashMap<String, Writes> = HashMap::new();
    let mut errors = Vec::new();
    let mut reads = Vec::with_capacity(layers.len());
    for (file, layer) in layers {
        let label = Config::config_label(file);
        let read = crate::shape::seeded::read(file, layer);
        errors.extend(read.errors.iter().cloned());
        for (namespace, tree) in &read.writes {
            let value = Value::from_json(tree).expect("TOML has no null");
            out.entry(namespace.clone())
                .or_default()
                .push((label.clone(), value));
        }
        reads.push(read);
    }
    errors.extend(plan_templates(layers, &reads, registry, &mut out));
    (out, errors)
}

/// The `<p>:template` writes: each provider's `template` line, composed over
/// every layer, rendered once per layer over that layer's own `[client]` tree.
/// A line that gives nothing for a layer writes nothing for it. A provider is
/// any the registry has or a root names: its template may come from a line of
/// its own, from a layer's `[client.providers]`, or from the abstract
/// provider's line, which reaches every provider there is.
fn plan_templates(
    layers: &[(PathBuf, toml::Value)],
    reads: &[crate::shape::seeded::Seeded],
    registry: &ProviderRegistry,
    out: &mut HashMap<String, Writes>,
) -> Vec<String> {
    let config = Config::from_layers(layers.to_vec());
    let empty = toml::Table::new();
    let providers = config.daemon().providers.as_table().unwrap_or(&empty);
    let lines = match crate::shape::provider::ProviderLines::new(providers) {
        Ok(lines) => lines,
        // The same errors the registry's load reports; nothing is rendered
        // from lines that do not load.
        Err(errors) => return errors,
    };

    let mut names: std::collections::BTreeSet<String> =
        registry.provider_names().into_iter().collect();
    names.extend(config.named_providers());
    names.retain(|name| crate::shape::seeded::provider_name_error("", name, true).is_none());

    let mut errors = Vec::new();
    for ((file, layer), read) in layers.iter().zip(reads) {
        let label = Config::config_label(file);
        let client = layer
            .get("client")
            .and_then(|c| serde_json::to_value(c).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        for name in names.iter().filter(|n| !read.fenced.contains(*n)) {
            match lines.template_tree(name, &client) {
                Ok(Some(tree)) => {
                    let value = Value::from_json(&tree).expect("a template tree has no null");
                    out.entry(format!("{name}:template"))
                        .or_default()
                        .push((label.clone(), value));
                }
                Ok(None) => {}
                Err(e) => errors.push(format!("{label}: {e}")),
            }
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(text: &str) -> (PathBuf, toml::Value) {
        (PathBuf::from("/f.toml"), toml::from_str(text).unwrap())
    }

    fn planned(layers: &[(PathBuf, toml::Value)]) -> HashMap<String, Writes> {
        plan(layers, &ProviderRegistry::new()).0
    }

    #[test]
    fn a_layer_seeds_only_the_templates_that_validated() {
        let layers = [
            crate::daemon::defaults::comb_layer(),
            layer(
                "[client.providers.bad]\ntemplate.n = 1\n\n[client.providers.ok]\ntemplate.n = \"{{ 1 }}\"\n",
            ),
        ];
        let (planned, errors) = plan(&layers, &ProviderRegistry::new());
        assert_eq!(errors.len(), 1, "the bad one is reported once: {errors:?}");
        assert!(
            errors[0].starts_with("config:/f.toml: [client.providers.bad] template 'n'"),
            "{errors:?}"
        );
        let mut namespaces: Vec<&str> = planned
            .keys()
            .map(String::as_str)
            .filter(|n| n.ends_with(":template"))
            .collect();
        namespaces.sort();
        assert_eq!(namespaces, ["ok:template"]);
    }

    #[test]
    fn each_layer_seeds_from_its_own_client_tree() {
        let layers = [
            crate::daemon::defaults::comb_layer(),
            layer("[client.providers.aws]\ntemplate.a = \"1\"\n"),
            layer("[client.providers.aws]\ntemplate.b = \"2\"\n"),
            layer("[daemon]\n"),
        ];
        let planned = planned(&layers);
        let writes = &planned["aws:template"];
        assert_eq!(
            writes.len(),
            2,
            "the layer with no client tree writes nothing"
        );
        assert_eq!(
            writes[0].1,
            Value::from_json(&serde_json::json!({"a": "1"})).unwrap()
        );
        assert_eq!(
            writes[1].1,
            Value::from_json(&serde_json::json!({"b": "2"})).unwrap()
        );
    }

    #[test]
    fn a_template_line_of_a_provider_reads_another_providers_client_tree() {
        let layers = [
            crate::daemon::defaults::comb_layer(),
            layer("[daemon.providers.git]\ntemplate = \"{{ client.providers.aws.template }}\"\n"),
            layer("[client.providers.aws]\ntemplate.region = \"{{ 1 }}\"\n"),
        ];
        let planned = planned(&layers);
        assert_eq!(planned["git:template"].len(), 1);
        assert_eq!(planned["aws:template"].len(), 1);
        assert_eq!(planned["git:template"][0].1, planned["aws:template"][0].1);
    }

    #[test]
    fn a_template_line_that_gives_a_non_table_is_reported_and_seeds_nothing() {
        let layers = [layer(
            "[daemon.providers.git]\ntemplate = \"{{ 1 }}\"\n\n[client.providers.git]\ntemplate.a = \"x\"\n",
        )];
        let (planned, errors) = plan(&layers, &ProviderRegistry::new());
        assert!(!planned.contains_key("git:template"), "{planned:?}");
        assert!(
            errors
                .iter()
                .any(|e| e.starts_with("config:/f.toml: git template: ")),
            "{errors:?}"
        );
    }

    #[test]
    fn a_custom_abstract_template_reaches_providers_no_file_names() {
        let layers = [
            crate::daemon::defaults::comb_layer(),
            layer("[daemon.providers._]\ntemplate = \"{{ {'where': '{{ 1 }}'} }}\"\n"),
        ];
        let planned = plan(&layers, &ProviderRegistry::with_defaults()).0;
        assert!(
            planned.contains_key("git:template"),
            "a built-in no file names has the abstract template: {:?}",
            planned.keys().collect::<Vec<_>>()
        );
    }

    /// A reload whose providers load but whose seed cannot be planned is
    /// abandoned whole: the registry in force, the cache and the scheduler are
    /// all as they were. The next sound one goes through.
    #[test]
    fn a_reload_whose_seed_cannot_be_planned_changes_nothing() {
        let config_of = |text: &str| {
            Config::from_layers(vec![crate::daemon::defaults::comb_layer(), layer(text)])
        };
        let cache = Arc::new(Cache::new());
        let registry = RegistryHandle::new(Arc::new(ProviderRegistry::with_defaults()));
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let scheduler = SchedulerHandle::new(tx);
        let before = registry.current();
        let mut last = before.clone();

        // A Source that loads, beside a `template` line that renders a number.
        let broken = config_of(
            "[daemon.providers.etest]\ntemplate = \"{{ 1 }}\"\n\n\
             [daemon.providers.etest.sources.u]\ndata = \"{{ 5 }}\"\nresult.u1 = \"{{ data }}\"\n",
        );
        let loaded = ProviderRegistry::load(&broken, &Default::default());
        assert!(
            loaded.errors.iter().any(|e| e.contains("etest template")),
            "the load that gates startup and the watch refuses it too: {:?}",
            loaded.errors
        );
        reload_providers(
            &cache,
            &registry,
            &scheduler,
            &mut last,
            &broken,
            loaded.registry,
        );
        assert!(Arc::ptr_eq(&before, &registry.current()));
        assert!(rx.try_recv().is_err(), "the scheduler is told nothing");
        assert!(cache.is_empty(), "and nothing is seeded");

        let sound = config_of(
            "[daemon.providers.etest.sources.u]\ndata = \"{{ 5 }}\"\nresult.u1 = \"{{ data }}\"\n",
        );
        let loaded = ProviderRegistry::load(&sound, &Default::default());
        reload_providers(
            &cache,
            &registry,
            &scheduler,
            &mut last,
            &sound,
            loaded.registry,
        );
        assert!(matches!(rx.try_recv(), Ok(SchedulerMessage::Reload { .. })));
    }
}
