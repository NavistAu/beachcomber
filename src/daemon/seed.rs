//! Seeding the config-seeded facets (spec §3.1, §3.2, §3.9).
//!
//! Three things in configuration are data the daemon writes into the cache,
//! and nothing else is seeded from config:
//!
//! - a provider's `template` line (`[daemon.providers.<p>] template`, by
//!   default `[client.providers.<p>] template` of the layer) → namespace
//!   `<p>:template`, whose string leaves are value expressions the client
//!   renders. The line is rendered here, once per layer, over that layer's
//!   `[client]` tree; the leaves themselves are never evaluated, but each is
//!   linted as the client template it is (`libbeachcomber::eval::lint`): one
//!   that does not compile is an error, and one reading a name no provider
//!   has is a warning, since a `put` may yet make that provider;
//! - `[daemon.providers.<p>.cache]` → namespace `<p>:cache` (`*:cache` for the
//!   abstract provider `<p> = _`), whose only key is the `_.decay` prototype;
//! - `[client.providers.<p>] macro.<name>` → namespace `<p>:macro`, one text
//!   per library, linted as the macro library it is: one that does not compile
//!   or prints is an error and is not written, and one importing a library no
//!   layer declares is a warning.
//!
//! What the `cache` and `macro` tables hold, and what is wrong with any file, comes from
//! one per-file parse (`crate::shape::seeded::read`). Each is written at the global path, every record labelled `config:<file>`
//! with the file that set it. Layers are written in composition order —
//! `defaults/*.toml` lexically, then the main config file, then
//! `conf.d/*.toml` lexically — and every file keeps its own records, a record
//! two files set included: a read takes it from the later file, and the
//! earlier file's is what it overrode, which `comb status --tree` shows.
//!
//! Settings still compose by deep merge. These tables deliberately do not: the
//! merge would collapse the layers into one value and the file a record came
//! from — the thing `comb status` shows as its source — would be gone before it
//! reached the cache.

use crate::cache::{CONFIG_SOURCE_PREFIX, Cache};
use crate::config::Config;
use crate::provider::Value;
use crate::provider::registry::{ConfigLoad, ProviderRegistry, RegistryHandle};
use crate::scheduler::{SchedulerHandle, SchedulerMessage};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// One namespace's writes: the `(label, tree)` pairs to apply in composition
/// order.
type Writes = Vec<(String, Value)>;

/// The seed of a configuration's layers: the writes per facet namespace, and
/// what is wrong with them. Planned once, by the load of the configuration
/// ([`ProviderRegistry::load`], which reports its errors and warnings), and
/// written by whoever puts that load in force: [`seed_facets`] at start,
/// [`reload_providers`] on a change.
#[derive(Default)]
pub struct Seed {
    pub(crate) writes: HashMap<String, Writes>,
    /// What stops the seed: a startup refusal, an abandoned reload.
    pub(crate) errors: Vec<String>,
    /// What is likely a mistake but is seeded all the same.
    pub(crate) warnings: Vec<String>,
}

/// Seed the config-seeded tables of `config`'s files, `seed` (its load's), and
/// watch them for changes: an edit under `providers` reloads the registry and
/// reseeds in place ([`reload_providers`]), a bare `[daemon]` or `[client]` key
/// cancels `cancel`.
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
/// daemon takes the mtime backstop alone, polled at that interval, because
/// registering a kernel fs-event watch costs about 1.2s per call on macOS and
/// a test that is not about the native backend should not pay it.
pub fn spawn_config_seeding(
    cache: &Arc<Cache>,
    registry: &RegistryHandle,
    config: &Config,
    seed: Seed,
    cancel: &CancellationToken,
    polling_watch: Option<Duration>,
    scheduler: &SchedulerHandle,
) {
    if config.config_file().is_none() {
        return;
    }
    seed_facets(cache, &registry.current(), seed);

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
        Some(interval) => (interval, false),
    };
    super::config_watch::spawn_config_self_watch_with(
        config,
        interval,
        fs_events,
        stop,
        move |kind, loaded| match kind {
            // What was compared and reloaded is what the gate validated.
            super::config_watch::ChangeKind::Providers => {
                reload_providers(&cache, &registry, &scheduler, &mut last, loaded);
            }
            super::config_watch::ChangeKind::Settings => {
                tracing::info!("configuration settings changed; shutting down for restart");
                restart.cancel();
            }
        },
    );
}

/// Apply a change under `[daemon.providers]` or `[client.providers]` in place.
/// `loaded` is what the gate loaded from the changed configuration: its
/// registry is made the one to serve from ([`ProviderRegistry::adopt`]) and
/// its seed, planned by that load, is written, and only when the seed is sound
/// does anything change: on any error the whole reload is abandoned, the
/// errors logged, the running registry and seed kept.
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
    loaded: ConfigLoad,
) {
    let next = loaded.registry.adopt(last, cache);
    // The warnings were the gate's to log, from the same load.
    let Seed {
        writes: planned,
        errors,
        ..
    } = loaded.seed;
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

/// Write `seed`, every seeded table of every layer, into the cache, one
/// `write_static` per (layer, namespace), registering its namespaces in
/// `registry`, the registry loaded with it. Used at daemon start, where startup
/// validation has already refused any file with an error; what a file with an
/// error would have seeded is not written, only what validated. Refusals are
/// logged.
pub fn seed_facets(cache: &Cache, registry: &ProviderRegistry, seed: Seed) {
    // The warnings are the startup load's to log.
    let Seed {
        writes: planned,
        errors,
        ..
    } = seed;
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

/// The seed of `config`'s layers: the write list per facet namespace, in
/// composition order, and every file's errors and warnings. `registry` is the
/// registry being loaded from `config`: it names the providers there are, and
/// its provider lines ([`ProviderRegistry::lines`]) are the ones the
/// `template` lines render from, compiled once by the load. Pure: registering
/// the namespaces is the caller's, so the seed can be planned and inspected
/// without touching the registry.
pub(crate) fn plan(config: &Config, registry: &ProviderRegistry) -> Seed {
    let mut plan = Seed::default();
    let layers: Vec<Layer<'_>> = config
        .composed_layers()
        .iter()
        .map(|(file, value)| Layer::of(file, value))
        .collect();
    for layer in &layers {
        plan.errors.extend(layer.read.errors.iter().cloned());
        for (namespace, tree) in &layer.read.writes {
            let value = Value::from_json(tree).expect("TOML has no null");
            plan.writes
                .entry(namespace.clone())
                .or_default()
                .push((layer.label.clone(), value));
        }
    }
    let world = ClientWorld::new(config, registry, &layers);
    plan_macros(&layers, &world, &mut plan);
    plan_templates(&layers, registry.lines(), &world, &mut plan);
    plan
}

/// One layer of a configuration as the plan reads it: the labels of its file,
/// each worked out once (a label looks `$HOME` up on disk), and what it seeds.
struct Layer<'c> {
    /// `config:<file>`: what its records are labelled.
    label: String,
    /// `config(<file>)`: what its errors and warnings are prefixed with.
    shown: String,
    value: &'c toml::Value,
    read: crate::shape::seeded::Seeded,
}

impl<'c> Layer<'c> {
    fn of(file: &Path, value: &'c toml::Value) -> Self {
        let shown = Config::error_label(file);
        Self {
            label: Config::config_label(file),
            read: crate::shape::seeded::read_labelled(shown.clone(), value),
            shown,
            value,
        }
    }
}

/// What a client text in a configuration is linted against: the client's
/// engine (the client renders it), every provider the registry has or a root
/// names, which a template reads by name, and every macro library a layer
/// declares, which a template imports by address.
struct ClientWorld {
    engine: libbeachcomber::eval::Probe<'static>,
    providers: Vec<String>,
    libraries: Vec<String>,
}

impl ClientWorld {
    fn new(config: &Config, registry: &ProviderRegistry, layers: &[Layer<'_>]) -> Self {
        let mut providers: std::collections::BTreeSet<String> =
            registry.provider_names().into_iter().collect();
        providers.extend(config.named_providers());
        providers
            .retain(|name| crate::shape::seeded::provider_name_error("", name, true).is_none());
        let mut libraries: Vec<String> = layers
            .iter()
            .flat_map(|layer| &layer.read.macros)
            .flat_map(|(provider, libraries)| {
                libraries
                    .keys()
                    .map(move |name| library_address(provider, name))
            })
            .collect();
        libraries.sort();
        libraries.dedup();
        Self {
            engine: libbeachcomber::eval::Probe::new(libbeachcomber::engine::build_engine_env(
                Default::default(),
            )),
            providers: providers.into_iter().collect(),
            libraries,
        }
    }

    fn known(&self) -> libbeachcomber::eval::Known<'_> {
        libbeachcomber::eval::Known {
            providers: &self.providers,
            libraries: &self.libraries,
        }
    }
}

/// The address of `provider`'s macro library `name`: `<p>:macro.<name>`.
fn library_address(provider: &str, name: &str) -> String {
    libbeachcomber::key::join(
        &format!("{provider}:macro"),
        &libbeachcomber::key::quote(name),
    )
}

/// The `<p>:macro` writes: each layer's `[client.providers.<p>] macro`
/// libraries, at the global path, labelled with the file, a later file
/// replacing one library and keeping its siblings. Each is linted as a macro
/// library: one that does not compile, or prints, is an error and is not
/// written; one that imports a library no layer declares is a warning.
fn plan_macros(layers: &[Layer<'_>], world: &ClientWorld, plan: &mut Seed) {
    use libbeachcomber::eval::{Finding, Line, lint};
    for Layer {
        label, shown, read, ..
    } in layers
    {
        for (provider, libraries) in &read.macros {
            let mut written = serde_json::Map::new();
            for (name, text) in libraries {
                let src = text.as_str().unwrap_or_default();
                let mut compiles = true;
                for finding in lint(&world.engine, src, Line::Macro, &world.known()) {
                    let message = format!(
                        "{shown}: [client.providers.{provider}] {}: {finding}",
                        libbeachcomber::key::push("macro", name)
                    );
                    match finding {
                        Finding::Compile(_) => {
                            compiles = false;
                            plan.errors.push(message);
                        }
                        Finding::Unknown(_) => plan.warnings.push(message),
                    }
                }
                if compiles {
                    written.insert(name.clone(), text.clone());
                }
            }
            if !written.is_empty() {
                let value = Value::from_json(&serde_json::Value::Object(written))
                    .expect("a library is a string");
                plan.writes
                    .entry(format!("{provider}:macro"))
                    .or_default()
                    .push((label.clone(), value));
            }
        }
    }
}

/// The `<p>:template` writes: each provider's `template` line in `lines`,
/// rendered once per layer over that layer's own `[client]` tree.
/// A line that gives nothing for a layer writes nothing for it. A provider is
/// any the registry has or a root names: its template may come from a line of
/// its own, from a layer's `[client.providers]`, or from the abstract
/// provider's line, which reaches every provider there is. Each leaf of a tree
/// is linted as a client template; a tree with a leaf that does not compile
/// is not written.
fn plan_templates(
    layers: &[Layer<'_>],
    lines: &crate::shape::provider::ProviderLines,
    world: &ClientWorld,
    plan: &mut Seed,
) {
    let known = world.known();
    for Layer {
        label,
        shown,
        value,
        read,
    } in layers
    {
        // The layer's `[client]` tree as the engine reads it, made once for
        // every provider's line.
        let client = minijinja::Value::from_serialize(
            value
                .get("client")
                .and_then(|c| serde_json::to_value(c).ok())
                .unwrap_or_else(|| serde_json::json!({})),
        );
        for name in world.providers.iter().filter(|n| !read.fenced.contains(*n)) {
            match lines.template_tree(name, &client) {
                Ok(Some(tree)) => {
                    let mut compiles = true;
                    for (leaf, finding) in
                        lint_template_tree(&world.engine, &tree, "template", &known)
                    {
                        let message =
                            format!("{shown}: [client.providers.{name}] {leaf}: {finding}");
                        match finding {
                            libbeachcomber::eval::Finding::Compile(_) => {
                                compiles = false;
                                plan.errors.push(message);
                            }
                            libbeachcomber::eval::Finding::Unknown(_) => {
                                plan.warnings.push(message)
                            }
                        }
                    }
                    if !compiles {
                        continue;
                    }
                    let value = Value::from_json(&tree).expect("a template tree has no null");
                    plan.writes
                        .entry(format!("{name}:template"))
                        .or_default()
                        .push((label.clone(), value));
                }
                Ok(None) => {}
                Err(e) => plan.errors.push(format!("{shown}: {e}")),
            }
        }
    }
}

/// What a `put` of `data` to `namespace` (`<p>:macro`) at `node` writes, checked
/// as a configured library is: each library a string under a name the name
/// rule allows, at `<p>:macro.<name>` or as a table of them at `<p>:macro`,
/// that compiles and prints nothing. A library importing one nothing declares
/// is taken, since a later put may declare it. `null` writes nothing to check.
pub fn check_macro_put(
    namespace: &str,
    node: &str,
    data: &serde_json::Value,
) -> Result<(), String> {
    use libbeachcomber::eval::{Finding, Known, Line, lint};
    let segments = libbeachcomber::key::segments(node);
    let libraries: Vec<(String, &serde_json::Value)> = match (segments.as_slice(), data) {
        ([], serde_json::Value::Object(map)) => map
            .iter()
            .map(|(name, text)| (name.clone(), text))
            .collect(),
        ([], _) => {
            return Err(format!(
                "{namespace}: a put writes a macro library at {namespace}.<name>, or a table \
                 of them"
            ));
        }
        ([name], text) => vec![(name.to_string(), text)],
        _ => {
            return Err(format!(
                "{}: a macro library is {namespace}.<name>",
                libbeachcomber::key::join(namespace, node)
            ));
        }
    };
    let engine = libbeachcomber::eval::Probe::new(libbeachcomber::engine::build_engine_env(
        Default::default(),
    ));
    for (name, text) in libraries.into_iter().filter(|(_, text)| !text.is_null()) {
        let at = libbeachcomber::key::join(namespace, &libbeachcomber::key::quote(&name));
        if let Some(e) = crate::shape::seeded::library_error(&name, text.as_str()) {
            return Err(format!("{at} {e}"));
        }
        let src = text.as_str().unwrap_or_default();
        if let Some(finding) = lint(&engine, src, Line::Macro, &Known::default())
            .into_iter()
            .find(|f| matches!(f, Finding::Compile(_)))
        {
            return Err(format!("{at}: {finding}"));
        }
    }
    Ok(())
}

/// Each string leaf of `tree`, a `:template` tree whose root is at `at`,
/// linted as a client template in a world of `known`, with its address under
/// `at` (`template.a.b` for a tree at `template`).
pub fn lint_template_tree(
    engine: &libbeachcomber::eval::Probe<'_>,
    tree: &serde_json::Value,
    at: &str,
    known: &libbeachcomber::eval::Known<'_>,
) -> Vec<(String, libbeachcomber::eval::Finding)> {
    fn walk(
        engine: &libbeachcomber::eval::Probe<'_>,
        node: &serde_json::Value,
        at: String,
        known: &libbeachcomber::eval::Known<'_>,
        out: &mut Vec<(String, libbeachcomber::eval::Finding)>,
    ) {
        match node {
            serde_json::Value::Object(map) => {
                for (key, sub) in map {
                    walk(engine, sub, libbeachcomber::key::push(&at, key), known, out);
                }
            }
            serde_json::Value::String(line) => out.extend(
                libbeachcomber::eval::lint(
                    engine,
                    line,
                    libbeachcomber::eval::Line::Template,
                    known,
                )
                .into_iter()
                .map(|finding| (at.clone(), finding)),
            ),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(engine, tree, at.to_string(), known, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn layer(text: &str) -> (PathBuf, toml::Value) {
        (PathBuf::from("/f.toml"), toml::from_str(text).unwrap())
    }

    /// The plan of `layers`, over the registry a load of them builds.
    fn plan_of(layers: &[(PathBuf, toml::Value)]) -> Seed {
        let config = Config::from_layers(layers.to_vec());
        let registry = ProviderRegistry::load(&config, &HashMap::new()).registry;
        plan(&config, &registry)
    }

    fn planned(layers: &[(PathBuf, toml::Value)]) -> HashMap<String, Writes> {
        plan_of(layers).writes
    }

    fn layer_at(file: &str, text: &str) -> (PathBuf, toml::Value) {
        (PathBuf::from(file), toml::from_str(text).unwrap())
    }

    /// The libraries each layer declares, as its writes to `<p>:macro`.
    fn macro_writes(plan: &Seed, provider: &str) -> Vec<(String, serde_json::Value)> {
        plan.writes
            .get(&format!("{provider}:macro"))
            .into_iter()
            .flatten()
            .map(|(label, value)| (label.clone(), serde_json::to_value(value).unwrap()))
            .collect()
    }

    /// Each layer writes its own libraries at `<p>:macro`, labelled with its
    /// file, so a later file replaces one library and keeps its siblings. A
    /// provider named only by its macros is a provider.
    #[test]
    fn a_layer_seeds_its_macro_libraries() {
        let layers = [
            layer_at(
                "/a.toml",
                "[client.providers.p]\nmacro.a = '{% macro a() %}A{% endmacro %}'\n\
                 macro.b = '{% macro b() %}B{% endmacro %}'\n",
            ),
            layer_at(
                "/b.toml",
                "[client.providers.p]\nmacro.b = '{% macro b() %}B2{% endmacro %}'\n",
            ),
        ];
        let plan = plan_of(&layers);
        assert_eq!(plan.errors, Vec::<String>::new());
        assert_eq!(plan.warnings, Vec::<String>::new());
        assert_eq!(
            macro_writes(&plan, "p"),
            [
                (
                    "config:/a.toml".to_string(),
                    serde_json::json!({
                        "a": "{% macro a() %}A{% endmacro %}",
                        "b": "{% macro b() %}B{% endmacro %}"
                    })
                ),
                (
                    "config:/b.toml".to_string(),
                    serde_json::json!({"b": "{% macro b() %}B2{% endmacro %}"})
                ),
            ]
        );
        assert!(
            Config::from_layers(layers.to_vec())
                .named_providers()
                .contains(&"p".to_string())
        );
    }

    /// A library that does not compile, or prints outside its macros, is a
    /// load error naming it, and is not written; its siblings are.
    #[test]
    fn a_library_that_does_not_compile_or_prints_is_a_load_error() {
        let layers = [layer(
            "[client.providers.p.macro]\n\
             broken = '{% macro m( %}'\n\
             text = 'hello{% macro m() %}{% endmacro %}'\n\
             tag = '{% macro m() %}{% endmacro %}{{ m() }}'\n\
             call = '{% macro m() %}{% endmacro %}{% call m() %}{% endcall %}'\n\
             ok = '''\n{%- set th = env.TH -%}\n{%- set block %}captured{% endset -%}\n\
             {%- macro m(x) -%}{{ x ~ th }}{%- endmacro %}\n'''\n",
        )];
        let plan = plan_of(&layers);
        let prints = "a macro library prints nothing: its text, {{ }} and {% call %} belong \
                      inside a {% macro %}";
        let mut errors = plan.errors.clone();
        errors.sort();
        assert_eq!(errors.len(), 4, "{errors:?}");
        assert!(
            errors[0].starts_with("config(/f.toml): [client.providers.p] macro.broken: "),
            "{errors:?}"
        );
        assert_eq!(
            errors[1..],
            [
                format!("config(/f.toml): [client.providers.p] macro.call: {prints}"),
                format!("config(/f.toml): [client.providers.p] macro.tag: {prints}"),
                format!("config(/f.toml): [client.providers.p] macro.text: {prints}"),
            ]
        );
        let written = macro_writes(&plan, "p");
        assert_eq!(written.len(), 1);
        let names: Vec<&String> = written[0].1.as_object().unwrap().keys().collect();
        assert_eq!(names, ["ok"]);
    }

    /// A library or a template that imports a library no layer declares is a
    /// warning, and is written: a later `put` may declare it.
    #[test]
    fn a_library_naming_an_unknown_one_is_a_warning() {
        let layers = [
            crate::daemon::defaults::comb_layer(),
            layer(
                "[client.providers.p]\n\
                 macro.a = '{% from \"p:macro.b\" import b %}{% macro a() %}{{ b() }}{% endmacro %}'\n\
                 macro.b = '{% from \"p:macro.zz\" import z %}{% macro b() %}{{ z() }}{% endmacro %}'\n\
                 template.t = '{% from \"q:macro.y\" import y %}{{ y() }}'\n\
                 template.u = '{% from \"p:macro.a\" import a %}{{ a() }}'\n",
            ),
        ];
        let plan = plan_of(&layers);
        assert_eq!(plan.errors, Vec::<String>::new());
        let mut warnings = plan.warnings.clone();
        warnings.sort();
        assert_eq!(
            warnings,
            [
                "config(/f.toml): [client.providers.p] macro.b: unknown macro library \
                 'p:macro.zz' (no layer declares it)",
                "config(/f.toml): [client.providers.p] template.t: unknown macro library \
                 'q:macro.y' (no layer declares it)",
            ]
        );
        assert_eq!(macro_writes(&plan, "p")[0].1.as_object().unwrap().len(), 2);
        assert!(plan.writes.contains_key("p:template"));
    }

    #[test]
    fn a_layer_seeds_only_the_templates_that_validated() {
        let layers = [
            crate::daemon::defaults::comb_layer(),
            layer(
                "[client.providers.bad]\ntemplate.n = 1\n\n[client.providers.ok]\ntemplate.n = \"{{ 1 }}\"\n",
            ),
        ];
        let Seed {
            writes: planned,
            errors,
            ..
        } = plan_of(&layers);
        assert_eq!(errors.len(), 1, "the bad one is reported once: {errors:?}");
        assert!(
            errors[0].starts_with("config(/f.toml): [client.providers.bad] template 'n'"),
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
        let Seed {
            writes: planned,
            errors,
            ..
        } = plan_of(&layers);
        assert!(!planned.contains_key("git:template"), "{planned:?}");
        assert!(
            errors
                .iter()
                .any(|e| e.starts_with("config(/f.toml): git template: ")),
            "{errors:?}"
        );
    }

    #[test]
    fn a_custom_abstract_template_reaches_providers_no_file_names() {
        let layers = [
            crate::daemon::defaults::comb_layer(),
            layer(
                "[daemon.providers._]\ntemplate = \"{{ {'where': '{{ 1 }}'} }}\"\n\
                 [daemon.providers.p.sources.s]\ndata = \"{{ 1 }}\"\nresult.x = \"{{ data }}\"\n",
            ),
        ];
        let planned = planned(&layers);
        assert!(
            planned.contains_key("p:template"),
            "a provider no client table names has the abstract template: {:?}",
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
        let registry = RegistryHandle::new(Arc::new(ProviderRegistry::shipped(
            &std::env::vars().collect(),
        )));
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let scheduler = SchedulerHandle::new(tx);
        let before = registry.current();
        let mut last = before.clone();

        // A Source that loads, beside a `template` line that renders a number.
        let broken = config_of(
            "[daemon.providers.etest]\ntemplate = \"{{ 1 }}\"\n\n\
             [daemon.providers.etest.sources.u]\ndata = \"{{ 5 }}\"\nresult.u1 = \"{{ data }}\"\n",
        );
        let loaded = ProviderRegistry::load(&broken, &std::env::vars().collect());
        assert!(
            loaded.errors.iter().any(|e| e.contains("etest template")),
            "the load that gates startup and the watch refuses it too: {:?}",
            loaded.errors
        );
        reload_providers(&cache, &registry, &scheduler, &mut last, loaded);
        assert!(Arc::ptr_eq(&before, &registry.current()));
        assert!(rx.try_recv().is_err(), "the scheduler is told nothing");
        assert!(cache.is_empty(), "and nothing is seeded");

        let sound = config_of(
            "[daemon.providers.etest.sources.u]\ndata = \"{{ 5 }}\"\nresult.u1 = \"{{ data }}\"\n",
        );
        let loaded = ProviderRegistry::load(&sound, &std::env::vars().collect());
        reload_providers(&cache, &registry, &scheduler, &mut last, loaded);
        assert!(matches!(rx.try_recv(), Ok(SchedulerMessage::Reload { .. })));
    }
}
