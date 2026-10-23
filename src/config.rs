use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Deep-merge `overlay` onto `base`: matching table keys recurse; everything
/// else (scalars, arrays, and table/non-table type mismatches) is last-wins
/// — `overlay`'s value replaces whatever was at that slot in `base`.
///
/// This is the composition primitive behind conf.d drop-ins (canon
/// `docs/roadmap.md` conf.d convention): a drop-in overriding one key of
/// `[daemon.providers.x]` must not clobber the rest of that block, which is exactly
/// what table recursion gives us — only overlapping keys are touched.
pub fn deep_merge_toml(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base_table), toml::Value::Table(overlay_table)) => {
            for (key, val) in overlay_table {
                match base_table.get_mut(&key) {
                    Some(existing) => deep_merge_toml(existing, val),
                    None => {
                        base_table.insert(key, val);
                    }
                }
            }
        }
        (base_slot, overlay_val) => {
            *base_slot = overlay_val;
        }
    }
}

/// Parse a duration string like "30s", "5m", "1h" or "50ms" into a Duration.
/// An `ms` suffix takes any whole number of milliseconds of at least 1
/// (`"1500ms"` is 1.5 s); `"0ms"` is `None`. Consumers that can only act on
/// whole seconds use [`parse_whole_seconds`].
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(stripped) = s.strip_suffix("ms") {
        let n = stripped.trim().parse::<u64>().ok()?;
        if n < 1 {
            return None;
        }
        return Some(Duration::from_millis(n));
    }
    let (num_str, multiplier) = if let Some(stripped) = s.strip_suffix('s') {
        (stripped, 1u64)
    } else if let Some(stripped) = s.strip_suffix('m') {
        (stripped, 60)
    } else if let Some(stripped) = s.strip_suffix('h') {
        (stripped, 3600)
    } else {
        (s, 1)
    };
    num_str
        .trim()
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(multiplier))
        .map(Duration::from_secs)
}

/// [`parse_duration`], but `None` unless the duration is a whole number of
/// seconds: what every seconds-only consumer (intervals, `put --ttl`) accepts.
pub fn parse_whole_seconds(s: &str) -> Option<Duration> {
    parse_duration(s).filter(|d| d.subsec_nanos() == 0)
}

/// The `[daemon] log_level` names, lowercase; the check accepts them in any
/// case.
pub const LOG_LEVELS: [&str; 6] = ["off", "error", "warn", "info", "debug", "trace"];

/// The filter a `log_level` names, or `None` if it is not one of
/// [`LOG_LEVELS`] (case aside).
pub fn log_level_filter(name: &str) -> Option<tracing_subscriber::filter::LevelFilter> {
    use tracing_subscriber::filter::LevelFilter;
    let level = name.to_ascii_lowercase();
    LOG_LEVELS.iter().position(|l| *l == level).map(|i| {
        [
            LevelFilter::OFF,
            LevelFilter::ERROR,
            LevelFilter::WARN,
            LevelFilter::INFO,
            LevelFilter::DEBUG,
            LevelFilter::TRACE,
        ][i]
    })
}

fn deserialize_log_level<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<tracing_subscriber::filter::LevelFilter, D::Error> {
    let name = String::deserialize(d)?;
    log_level_filter(&name).ok_or_else(|| {
        serde::de::Error::custom(format!(
            "log_level must be one of {}",
            LOG_LEVELS.join(", ")
        ))
    })
}

/// Which resolution step supplied a socket path
/// (canon singleton.md §"Canonical socket path resolution").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketPathSource {
    ConfigOverride,
    EnvVar,
    Default,
}

/// The configuration: settings composed from layers, and everything wrong
/// with them. Built only by [`Config::load_from`], [`Config::from_layers`] and
/// [`Config::from_toml`] (or `Default`, which is valid by construction), so
/// every `Config` there is has had the same checks run on it; its settings are
/// read through [`Config::daemon`] and [`Config::client`].
#[derive(Debug, Clone)]
pub struct Config {
    daemon: DaemonConfig,
    /// `[client]`: what a client reads from the daemon's configuration; today
    /// only `providers` (`[client.providers.<p>] template.<leaf> = "…"`, seeded
    /// as `<p>:template.<leaf>`). A `toml::Value`, so that a `[client]` of the
    /// wrong type still composes and the per-file check can report it.
    client: toml::Value,
    /// The `config.toml` this config was loaded from, whether or not the file
    /// exists — `Config::conf_d_dir_for` gives the other half of the composed
    /// set, so the filename is spelled in one place. `Some` only for a
    /// `Config::load_from` (and so `Config::load`); `None` for one built from
    /// layers or in memory, which corresponds to no file at all.
    ///
    /// The daemon seeds the config-seeded facets and watches for changes only when this is
    /// `Some`, so an embedded or test daemon handed a hand-built `Config` reads
    /// nothing from the ambient `~/.config/beachcomber`.
    config_path: Option<PathBuf>,
    /// The layers this configuration was composed from, in composition order:
    /// the one source of truth. `errors()` were found in them and the daemon
    /// seeds its facets from them, so an edit after the load cannot slip past
    /// the check or be seeded unchecked. Empty for the default.
    layers: Vec<(PathBuf, toml::Value)>,
    /// Everything wrong with `layers`, found once when they were composed: a
    /// file that could not be read or parsed, a rule a file breaks, a value
    /// the composition cannot hold.
    errors: Vec<String>,
}

/// What the composed layers deserialize to: the two roots. Private, so the
/// only way to a `Config` is through `compose`.
#[derive(Deserialize)]
#[serde(default)]
struct Composed {
    daemon: DaemonConfig,
    #[serde(default = "empty_table")]
    client: toml::Value,
}

impl Default for Composed {
    fn default() -> Self {
        Self {
            daemon: DaemonConfig::default(),
            client: empty_table(),
        }
    }
}

/// The configuration files that compose, each parsed on its own, and the
/// errors of those that could not be: [`Config::read_layers`]'s one answer.
#[derive(Debug, Default)]
pub struct ReadLayers {
    /// In composition order, labelled by the file each came from.
    pub layers: Vec<(PathBuf, toml::Value)>,
    /// One per file that exists but could not be read or parsed, each prefixed
    /// with the file's `config:<path>` label.
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    pub socket_path: Option<String>,
    /// One of [`LOG_LEVELS`], any case; the configuration check refuses
    /// anything else, so a `Config` that loaded holds a valid level.
    #[serde(deserialize_with = "deserialize_log_level")]
    pub log_level: tracing_subscriber::filter::LevelFilter,
    /// Seconds a Source may run before it is cancelled; at least 1.
    pub provider_timeout_secs: u64,
    /// Path to an env file loaded at daemon startup.
    /// Each line is KEY=VALUE (or KEY="VALUE"). Blank lines and #comments are ignored.
    /// These vars are injected into the daemon's environment before any providers execute,
    /// making them reach a Source's lines as `env.VAR` (an unset one renders empty)
    /// and the commands its `exec()` runs.
    /// Default: `env` beside the config file.
    pub env_file: Option<String>,
    /// How often the watchdog checks the scheduler heartbeat. Default: disabled (None).
    /// Whole seconds, at least 1s. Example: "30s", "1m".
    pub watchdog_interval: Option<String>,
    /// How long the heartbeat can be stale before the watchdog restarts the scheduler.
    /// Default: 3x watchdog_interval; never below the interval, and not set
    /// without one.
    /// Example: "90s", "3m".
    pub watchdog_threshold: Option<String>,
    /// Seconds with no client before the daemon shuts down. Default: never.
    pub idle_shutdown_secs: Option<u64>,
    /// `[daemon.budget]`: `execute`, the execute budget recorded for every
    /// Source whose own `return` does not name `budget(...)`
    /// (`DaemonConfig::budget_execute`); nothing enforces it yet. A
    /// `toml::Value` for the reason `Config::client` is one.
    #[serde(default = "empty_table")]
    pub budget: toml::Value,
    /// `[daemon.providers.<p>]`: what the daemon holds about a provider:
    /// `cache` (`[daemon.providers.<p>.cache._] decay = N`, seeded as
    /// `<p>:cache._.decay`, and `*:cache` for `<p> = _`) and `sources`
    /// (`[daemon.providers.<p>.sources.<s>]`, built by `crate::shape::build`). A `toml::Value` for
    /// the reason `Config::client` is one; the per-file check is
    /// `Config::errors`.
    #[serde(default = "empty_table")]
    pub providers: toml::Value,
    /// `[daemon.plugin]`: where `plugin(name, ...)` finds libraries, `path`
    /// only (`Config::plugin_path`). A `toml::Value` for the reason
    /// `Config::client` is one.
    #[serde(default = "empty_table")]
    pub plugin: toml::Value,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            daemon: DaemonConfig::default(),
            client: empty_table(),
            config_path: None,
            layers: Vec::new(),
            errors: Vec::new(),
        }
    }
}

fn empty_table() -> toml::Value {
    toml::Value::Table(toml::Table::new())
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: None,
            log_level: tracing_subscriber::filter::LevelFilter::INFO,
            provider_timeout_secs: 10,
            env_file: None,
            watchdog_interval: None,
            watchdog_threshold: None,
            idle_shutdown_secs: None,
            budget: empty_table(),
            providers: empty_table(),
            plugin: empty_table(),
        }
    }
}

impl DaemonConfig {
    /// Whole seconds, at least 1: a zero interval would panic the timer, so
    /// the accessor refuses it as well as the configuration check.
    pub fn watchdog_interval_duration(&self) -> Option<Duration> {
        self.watchdog_interval
            .as_ref()
            .and_then(|s| parse_whole_seconds(s))
            .filter(|d| !d.is_zero())
    }

    pub fn watchdog_threshold_duration(&self) -> Option<Duration> {
        if let Some(ref s) = self.watchdog_threshold {
            parse_whole_seconds(s).filter(|d| !d.is_zero())
        } else {
            // Default: 3x the watchdog interval
            self.watchdog_interval_duration().map(|d| d * 3)
        }
    }

    /// `[daemon] budget.execute`: how long a Source may execute before it is
    /// over budget, for every Source whose `return` does not say. A value of
    /// the wrong shape is `Config::errors`'s to report; it reads as
    /// absent here.
    pub fn budget_execute(&self) -> Duration {
        self.budget
            .get("execute")
            .and_then(toml::Value::as_str)
            .and_then(parse_duration)
            .unwrap_or(crate::scheduler::lifecycle::DEFAULT_BUDGET_EXECUTE)
    }
}

impl Config {
    /// The directories `plugin(name, ...)` searches for `lib<name>.<ext>`, in
    /// order: `[daemon.plugin] path` with a leading `~` written as `home`, or,
    /// when the list is not given, the `plugins` directory beside the config
    /// file (none for a `Config` built in memory, which reads nothing ambient).
    /// A list is replaced whole by a later layer, as any array is. A value of
    /// the wrong shape is `Config::errors`'s to report; it reads as
    /// absent here.
    pub fn plugin_path(&self, home: Option<&str>) -> Result<Vec<PathBuf>, String> {
        let listed: Option<Vec<&str>> = self
            .daemon
            .plugin
            .get("path")
            .and_then(toml::Value::as_array)
            .and_then(|list| list.iter().map(toml::Value::as_str).collect());
        match listed {
            Some(dirs) => dirs
                .into_iter()
                .map(|dir| {
                    libbeachcomber::engine::expand_tilde(dir, home)
                        .map_err(|e| format!("[daemon.plugin] path entry {e}"))
                })
                .collect(),
            None => Ok(self
                .config_path
                .as_deref()
                .and_then(Path::parent)
                .map(|dir| vec![dir.join("plugins")])
                .unwrap_or_default()),
        }
    }

    /// The configuration at the default location: [`Self::load_from`] the
    /// config home's `config.toml`.
    pub fn load() -> Self {
        Self::load_from(&Self::config_path())
    }

    /// Compose every `defaults/*.toml` beside `config_path`, then the file
    /// itself, then every `conf.d/*.toml`, each directory in lexical filename
    /// order, each layer deep-merged onto the accumulated value. `config_path`
    /// is recorded even when no file exists there: it is where this config
    /// resolved to, and a conf.d-only installation is legitimate.
    ///
    /// Always returns a `Config`, and every problem with the files (one that
    /// cannot be read or parsed, a rule it breaks, a value the composition
    /// cannot hold) is in [`Self::errors`]: the daemon refuses to
    /// start on it, `comb check config` fails and the config self-watch does
    /// not restart into it. The settings of a composition that does not load
    /// are the defaults.
    pub fn load_from(config_path: &Path) -> Self {
        let read = Self::read_layers(
            Some(&Self::defaults_dir_for(config_path)),
            Some(config_path),
            &Self::conf_d_dir_for(config_path),
        );
        Self::compose(read, Some(config_path.to_path_buf()))
    }

    /// The configuration `layers` compose to, in order, with no file behind
    /// it: what an embedder or a test builds. The one constructor besides
    /// [`Self::load_from`], so the same checks run on every `Config`.
    pub fn from_layers(layers: Vec<(PathBuf, toml::Value)>) -> Self {
        Self::compose(
            ReadLayers {
                layers,
                errors: Vec::new(),
            },
            None,
        )
    }

    fn compose(read: ReadLayers, config_path: Option<PathBuf>) -> Self {
        let ReadLayers { layers, mut errors } = read;
        errors.extend(Self::validate_layers(&layers));
        let composed = match Composed::deserialize(Self::merged(&layers)) {
            Ok(composed) => composed,
            Err(e) => {
                if errors.is_empty() {
                    errors.push(format!(
                        "config: the composed configuration does not load: {e}"
                    ));
                }
                Composed::default()
            }
        };
        let mut config = Config {
            daemon: composed.daemon,
            client: composed.client,
            config_path,
            layers,
            errors: Vec::new(),
        };
        if errors.is_empty() {
            errors.extend(config.watchdog_errors());
        }
        config.errors = errors;
        config
    }

    fn merged(layers: &[(PathBuf, toml::Value)]) -> toml::Value {
        let mut composed = toml::Value::Table(Default::default());
        for (_, layer) in layers {
            deep_merge_toml(&mut composed, layer.clone());
        }
        composed
    }

    /// A configuration from the text of one layer that has no file, labelled
    /// `config:<in memory>`: what an embedder or a test builds. Text that is
    /// not TOML is an error in [`Self::errors`], as a file's would be.
    pub fn from_toml(text: &str) -> Self {
        let path = PathBuf::from("<in memory>");
        match toml::from_str::<toml::Value>(text) {
            Ok(layer) => Self::from_layers(vec![(path, layer)]),
            Err(e) => Self::compose(
                ReadLayers {
                    layers: Vec::new(),
                    errors: vec![Self::parse_error(&path, &e)],
                },
                None,
            ),
        }
    }

    /// The `[daemon]` settings.
    pub fn daemon(&self) -> &DaemonConfig {
        &self.daemon
    }

    /// `[client]`, as the value it composed to.
    pub fn client(&self) -> &toml::Value {
        &self.client
    }

    /// The `config.toml` path this configuration was loaded from (the file
    /// may not exist; see [`Self::loaded_path`]).
    pub fn config_file(&self) -> Option<&Path> {
        self.config_path.as_deref()
    }

    /// One line for a TOML parse error: the file's label and the error's own
    /// first line, which carries the line and column. Verdict lists show one
    /// error per line.
    fn parse_error(file: &Path, e: &toml::de::Error) -> String {
        let text = e.to_string();
        format!(
            "{}: {}",
            Self::config_label(file),
            text.lines().next().unwrap_or_default().trim_end()
        )
    }

    /// The layers this configuration was composed from, for seeding.
    pub fn composed_layers(&self) -> &[(PathBuf, toml::Value)] {
        &self.layers
    }

    /// The configuration files that compose, in composition order — every
    /// `defaults/*.toml` (when `defaults` is given), then the main config file
    /// (when `config_path` is given), then every `conf.d/*.toml`, each
    /// directory in lexical filename order — each parsed on its own. A file
    /// that does not exist is nothing (a conf.d-only installation is
    /// legitimate); one that cannot be read or does not parse is an error
    /// beside the layers that did.
    ///
    /// Settings compose by deep merge, but facet tables are seeded per layer so
    /// each record keeps the label of the file that set it — which is only
    /// possible while the layers are still separable.
    pub fn read_layers(
        defaults: Option<&Path>,
        config_path: Option<&Path>,
        conf_d: &Path,
    ) -> ReadLayers {
        let mut files: Vec<PathBuf> = Vec::new();
        if let Some(d) = defaults {
            files.extend(Self::conf_d_files(d));
        }
        if let Some(p) = config_path {
            files.push(p.to_path_buf());
        }
        files.extend(Self::conf_d_files(conf_d));
        let mut out = ReadLayers::default();
        for file in files {
            let content = match std::fs::read_to_string(&file) {
                Ok(c) => c,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    out.errors.push(format!(
                        "{}: could not read: {e}",
                        Self::config_label(&file)
                    ));
                    continue;
                }
            };
            match toml::from_str::<toml::Value>(&content) {
                Ok(layer) => out.layers.push((file, layer)),
                Err(e) => out.errors.push(Self::parse_error(&file, &e)),
            }
        }
        out
    }

    /// Everything wrong with the layers this configuration was composed from:
    /// a file that could not be read or parsed, then, in the two roots the
    /// daemon seeds from and the bare `[daemon]` keys
    /// (`crate::shape::seeded::read`), the rules each file breaks — checked
    /// per file because seeding is per file, so a rule broken in one file is
    /// not hidden by another file's value winning the merge — then what only
    /// the composition shows (the watchdog's two values against each other).
    pub fn errors(&self) -> Vec<String> {
        self.errors.clone()
    }

    /// The per-file rules over `layers`.
    pub fn validate_layers(layers: &[(PathBuf, toml::Value)]) -> Vec<String> {
        layers
            .iter()
            .flat_map(|(file, layer)| crate::shape::seeded::read(file, layer).errors)
            .collect()
    }

    /// What the composed watchdog values say together, naming the file that
    /// set each: a threshold without an interval has nothing to check against,
    /// and one below the interval would restart a healthy scheduler.
    fn watchdog_errors(&self) -> Vec<String> {
        let set_by = |key: &str| {
            self.layers
                .iter()
                .rev()
                .find(|(_, layer)| layer.get("daemon").and_then(|d| d.get(key)).is_some())
                .map(|(file, _)| Self::config_label(file))
                .unwrap_or_default()
        };
        let (interval, threshold) = (
            self.daemon.watchdog_interval_duration(),
            self.daemon.watchdog_threshold.as_deref(),
        );
        match (interval, threshold) {
            (None, Some(t)) => vec![format!(
                "{}: [daemon] watchdog_threshold ({t}) is set without watchdog_interval: the \
                 watchdog is off",
                set_by("watchdog_threshold")
            )],
            (Some(i), Some(_))
                if self
                    .daemon
                    .watchdog_threshold_duration()
                    .is_some_and(|t| t < i) =>
            {
                vec![format!(
                    "[daemon] watchdog_threshold ({}, set in {}) is below watchdog_interval \
                     ({}, set in {}): the watchdog would restart a healthy scheduler",
                    threshold.unwrap_or_default(),
                    set_by("watchdog_threshold"),
                    self.daemon.watchdog_interval.as_deref().unwrap_or_default(),
                    set_by("watchdog_interval"),
                )]
            }
            _ => Vec::new(),
        }
    }

    /// The names the two roots give providers, `_` excluded: a provider exists
    /// if either root names it. The registry registers those no code or source
    /// block already stands behind, with zero Sources.
    pub fn named_providers(&self) -> Vec<String> {
        let mut names = Vec::new();
        for (root, client) in [(&self.client, true), (&self.daemon.providers, false)] {
            // `[client]` nests the table under `providers`; `[daemon.providers]` is it.
            let table = if client {
                root.get("providers")
            } else {
                Some(root)
            };
            for name in table
                .and_then(|t| t.as_table())
                .into_iter()
                .flat_map(|t| t.keys())
            {
                if name != "_"
                    && crate::shape::seeded::provider_name_error("", name, client).is_none()
                {
                    names.push(name.clone());
                }
            }
        }
        names.sort();
        names.dedup();
        names
    }

    /// The composed value with the config-seeded tables removed: the half of
    /// configuration a change has to touch for a restart to be warranted. That
    /// is `[client.providers]` and every `[daemon.providers.<p>.cache]` table.
    /// The config self-watch compares this before and after
    /// (`crate::daemon::config_watch::ChangeKind`). Only the two roots exist
    /// (`crate::shape::seeded::read` refuses any other), so nothing else is kept.
    pub fn settings_only(mut composed: toml::Value) -> toml::Value {
        if let Some(table) = composed.as_table_mut() {
            table.retain(|root, _| root == "client" || root == "daemon");
        }
        if let Some(client) = composed.get_mut("client").and_then(|v| v.as_table_mut()) {
            client.remove("providers");
            if client.is_empty() {
                composed.as_table_mut().map(|t| t.remove("client"));
            }
        }
        if let Some(daemon) = composed.get_mut("daemon").and_then(|v| v.as_table_mut()) {
            if let Some(providers) = daemon.get_mut("providers").and_then(|v| v.as_table_mut()) {
                for (_, provider) in providers.iter_mut() {
                    if let Some(table) = provider.as_table_mut() {
                        table.remove("cache");
                    }
                }
                providers.retain(|_, v| !v.as_table().is_some_and(|t| t.is_empty()));
            }
            if daemon
                .get("providers")
                .and_then(|v| v.as_table())
                .is_some_and(|t| t.is_empty())
            {
                daemon.remove("providers");
            }
        }
        composed
    }

    /// The static source label for a record seeded from `path`: `config:` plus
    /// the file's path with `$HOME` written as `~`
    /// (`config:~/.config/beachcomber/conf.d/10-mine.toml`).
    pub fn config_label(path: &Path) -> String {
        let shown = crate::query::home_dir()
            .and_then(|home| path.strip_prefix(&home).ok().map(|rest| rest.to_path_buf()))
            .map(|rest| format!("~/{}", rest.display()))
            .unwrap_or_else(|| path.display().to_string());
        format!("{}{shown}", crate::cache::CONFIG_SOURCE_PREFIX)
    }

    /// The conf.d directory for a given main config file path:
    /// `<config-file-parent>/conf.d`. Derived from the path itself, not from
    /// whether the config file (or the conf.d directory) actually exists.
    pub fn conf_d_dir_for(config_path: &Path) -> PathBuf {
        config_path
            .parent()
            .map(|p| p.join("conf.d"))
            .unwrap_or_else(|| PathBuf::from("conf.d"))
    }

    /// The managed-defaults directory for a given main config file path:
    /// `<config-file-parent>/defaults`, the lowest layer of composition and the
    /// directory the daemon writes the shipped templates into
    /// (`crate::daemon::defaults`). Derived like `conf_d_dir_for`.
    pub fn defaults_dir_for(config_path: &Path) -> PathBuf {
        config_path
            .parent()
            .map(|p| p.join("defaults"))
            .unwrap_or_else(|| PathBuf::from("defaults"))
    }

    /// List `*.toml` files directly inside `dir`, sorted in lexical filename
    /// order (spec: drop-ins compose "in lexical filename order"). Returns an
    /// empty list if `dir` doesn't exist or can't be read — a conf.d
    /// directory is optional. The same listing serves the `defaults/`
    /// directory, which composes by the same rule.
    pub fn conf_d_files(dir: &Path) -> Vec<PathBuf> {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("toml"))
            .collect();
        files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        files
    }

    /// Return the default config file path (may not exist).
    pub fn config_path() -> std::path::PathBuf {
        let xdg = xdg::BaseDirectories::with_prefix("beachcomber");
        xdg.get_config_home()
            .unwrap_or_else(|| {
                let home = std::env::var("HOME").unwrap_or_default();
                std::path::PathBuf::from(format!("{home}/.config/beachcomber"))
            })
            .join("config.toml")
    }

    /// The `config.toml` this configuration was loaded from, if the file
    /// exists: the path status and `introspect` report. Never a file elsewhere.
    pub fn loaded_path(&self) -> Option<&Path> {
        self.config_path.as_deref().filter(|p| p.is_file())
    }

    /// Load environment variables from the configured env file (or default path).
    /// Sets them in the process environment, so they reach a Source's lines as
    /// `env.VAR` (an unset one renders empty) and the commands its `exec()` runs.
    /// Returns the number of variables loaded.
    pub fn load_env_file(&self) -> usize {
        let path = match &self.daemon.env_file {
            Some(p) => PathBuf::from(shellexpand(p)),
            None => {
                // Default: the variables file beside the config file; a
                // `Config` with no file behind it reads nothing ambient.
                match self.config_path.as_deref() {
                    Some(path) => path.with_file_name("env"),
                    None => return 0,
                }
            }
        };

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return 0,
        };

        let mut count = 0;
        for line in content.lines() {
            let line = line.trim();

            // Skip blanks and comments
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let value = value.trim();

                // Strip surrounding quotes if present
                let value = if (value.starts_with('"') && value.ends_with('"'))
                    || (value.starts_with('\'') && value.ends_with('\''))
                {
                    &value[1..value.len() - 1]
                } else {
                    value
                };

                // SAFETY: env file is loaded once at daemon startup before any threads
                // are spawned, so there are no concurrent readers of the environment.
                unsafe {
                    std::env::set_var(key, value);
                }
                count += 1;
            }
        }
        count
    }

    pub fn resolve_socket_path(&self) -> PathBuf {
        self.resolve_socket_path_with_source().0
    }

    /// Full canonical resolution, also reporting which step supplied the path.
    /// Auto-spawn uses the source to flag `$BEACHCOMBER_SOCKET`-derived daemons
    /// `--no-reap` (canon singleton.md §"Env-override spawns are flagged").
    pub fn resolve_socket_path_with_source(&self) -> (PathBuf, SocketPathSource) {
        // 1. Config-file override (highest priority)
        if let Some(ref path) = self.daemon.socket_path {
            return (PathBuf::from(path), SocketPathSource::ConfigOverride);
        }

        // 2. BEACHCOMBER_SOCKET env var
        if let Some(env_path) = std::env::var_os("BEACHCOMBER_SOCKET") {
            let p = std::path::PathBuf::from(env_path);
            if !p.as_os_str().is_empty() {
                return (p, SocketPathSource::EnvVar);
            }
        }

        // 3. Stable per-user default. Consults no session-scoped environment
        // (TMPDIR, XDG_RUNTIME_DIR): singleton enforcement is per-socket-path,
        // so session-scoped inputs yield one daemon per session, not per user.
        // See docs/canon/singleton.md §"Canonical socket path resolution".
        (Self::default_socket_path(), SocketPathSource::Default)
    }

    /// Env-free resolution backing the reaper role (canon singleton.md §"Who
    /// reaps"): config override → per-user default. `$BEACHCOMBER_SOCKET` is
    /// deliberately excluded — it is per-process, and honoring it here would
    /// let two daemons of one uid each self-assess "canonical" and reap each
    /// other (fratricide).
    pub fn resolve_reaper_socket_path(&self) -> PathBuf {
        if let Some(ref path) = self.daemon.socket_path {
            return PathBuf::from(path);
        }
        Self::default_socket_path()
    }

    pub(crate) fn default_socket_path() -> PathBuf {
        let uid = unsafe { libc::getuid() };
        PathBuf::from("/tmp")
            .join(format!("beachcomber-{uid}"))
            .join("sock")
    }

    pub fn resolve_log_path(&self) -> PathBuf {
        let xdg = xdg::BaseDirectories::with_prefix("beachcomber");

        xdg.get_state_home()
            .unwrap_or_else(|| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
                PathBuf::from(home)
                    .join(".local")
                    .join("state")
                    .join("beachcomber")
            })
            .join("daemon.log")
    }
}

/// Expand ~ to $HOME in a path string.
fn shellexpand(path: &str) -> String {
    if path.starts_with("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{}{}", home, &path[1..]);
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beachcomber_socket_env_var_takes_precedence_over_xdg() {
        temp_env::with_vars(
            [
                ("BEACHCOMBER_SOCKET", Some("/explicit/test/path.sock")),
                ("XDG_RUNTIME_DIR", Some("/should/be/ignored")),
            ],
            || {
                let cfg = Config::default();
                let path = cfg.resolve_socket_path();
                assert_eq!(path, std::path::PathBuf::from("/explicit/test/path.sock"));
            },
        );
    }

    #[test]
    fn config_file_socket_path_takes_precedence_over_env() {
        let cfg = Config::from_toml("[daemon]\nsocket_path = \"/from/config/file.sock\"\n");
        temp_env::with_var("BEACHCOMBER_SOCKET", Some("/from/env.sock"), || {
            let path = cfg.resolve_socket_path();
            assert_eq!(path, std::path::PathBuf::from("/from/config/file.sock"));
        });
    }

    // ── deep_merge_toml unit tests ────────────────────────────────────────

    fn toml_val(s: &str) -> toml::Value {
        toml::from_str(s).unwrap()
    }

    #[test]
    fn deep_merge_recurses_into_matching_tables() {
        let mut base = toml_val("[daemon]\nlog_level = \"info\"\nsocket_path = \"/a\"\n");
        let overlay = toml_val("[daemon]\nlog_level = \"debug\"\n");
        deep_merge_toml(&mut base, overlay);
        let daemon = base.get("daemon").unwrap();
        assert_eq!(daemon.get("socket_path").unwrap().as_str(), Some("/a"));
        assert_eq!(daemon.get("log_level").unwrap().as_str(), Some("debug"));
    }
    #[test]
    fn deep_merge_scalar_is_last_wins() {
        let mut base = toml_val("x = 1\n");
        let overlay = toml_val("x = 2\n");
        deep_merge_toml(&mut base, overlay);
        assert_eq!(base.get("x").unwrap().as_integer(), Some(2));
    }

    #[test]
    fn deep_merge_array_is_last_wins_not_appended() {
        let mut base = toml_val("arr = [1, 2, 3]\n");
        let overlay = toml_val("arr = [9]\n");
        deep_merge_toml(&mut base, overlay);
        let arr = base.get("arr").unwrap().as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].as_integer(), Some(9));
    }

    #[test]
    fn deep_merge_adds_new_keys_without_touching_siblings() {
        let mut base = toml_val("[a]\nb = 1\n");
        let overlay = toml_val("[a]\nc = 2\n[d]\ne = 3\n");
        deep_merge_toml(&mut base, overlay);
        assert_eq!(
            base.get("a").unwrap().get("b").unwrap().as_integer(),
            Some(1)
        );
        assert_eq!(
            base.get("a").unwrap().get("c").unwrap().as_integer(),
            Some(2)
        );
        assert_eq!(
            base.get("d").unwrap().get("e").unwrap().as_integer(),
            Some(3)
        );
    }

    // ── conf_d_files ordering ─────────────────────────────────────────────

    #[test]
    fn conf_d_files_are_sorted_lexically_by_filename() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("10-a.toml"), "").unwrap();
        std::fs::write(tmp.path().join("2-b.toml"), "").unwrap();
        std::fs::write(tmp.path().join("z.toml"), "").unwrap();
        std::fs::write(tmp.path().join("not-toml.txt"), "").unwrap();
        let files = Config::conf_d_files(tmp.path());
        let names: Vec<_> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        // Lexical, not numeric: "10-a.toml" < "2-b.toml" because '1' < '2'.
        assert_eq!(names, vec!["10-a.toml", "2-b.toml", "z.toml"]);
    }

    #[test]
    fn conf_d_files_missing_dir_is_empty() {
        let files = Config::conf_d_files(std::path::Path::new("/no/such/conf.d/dir"));
        assert!(files.is_empty());
    }

    // ── load_from / read_layers ─────────────────────────────────────────

    fn config_dir() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let conf_d = tmp.path().join("conf.d");
        std::fs::create_dir(&conf_d).unwrap();
        let main = tmp.path().join("config.toml");
        (tmp, main, conf_d)
    }

    #[test]
    fn load_from_drop_in_overrides_one_key_rest_of_block_preserved() {
        let (_tmp, main, conf_d) = config_dir();
        std::fs::write(
            conf_d.join("01-override.toml"),
            "[daemon]\nlog_level = \"debug\"\n",
        )
        .unwrap();
        std::fs::write(
            &main,
            "[daemon]\nlog_level = \"warn\"\nsocket_path = \"/s\"\n",
        )
        .unwrap();
        let cfg = Config::load_from(&main);
        assert_eq!(
            cfg.daemon().log_level,
            tracing_subscriber::filter::LevelFilter::from_level(tracing::Level::DEBUG)
        );
        assert_eq!(cfg.daemon().socket_path.as_deref(), Some("/s"));
        assert_eq!(cfg.config_file(), Some(main.as_path()));
    }

    #[test]
    fn load_from_conf_d_with_no_main_file() {
        let (_tmp, main, conf_d) = config_dir();
        std::fs::write(
            conf_d.join("01-daemon.toml"),
            "[daemon]\nlog_level = \"debug\"\n",
        )
        .unwrap();
        assert_eq!(
            Config::load_from(&main).daemon().log_level,
            tracing_subscriber::filter::LevelFilter::DEBUG
        );
    }

    #[test]
    fn load_from_skips_an_invalid_drop_in_and_composes_the_rest() {
        let (_tmp, main, conf_d) = config_dir();
        std::fs::write(conf_d.join("01-bad.toml"), "this is not [ valid toml").unwrap();
        std::fs::write(
            conf_d.join("02-good.toml"),
            "[daemon]\nlog_level = \"debug\"\n",
        )
        .unwrap();
        assert_eq!(
            Config::load_from(&main).daemon().log_level,
            tracing_subscriber::filter::LevelFilter::DEBUG
        );
    }

    #[test]
    fn a_toml_syntax_error_is_a_load_error_and_the_rest_still_composes() {
        let (_tmp, main, conf_d) = config_dir();
        std::fs::write(&main, "[daemon]\nlog_level = \"debug\"\n").unwrap();
        std::fs::write(conf_d.join("01-bad.toml"), "[daemon]\nsocket_path = \n").unwrap();
        let config = Config::load_from(&main);
        assert_eq!(
            config.daemon().log_level,
            tracing_subscriber::filter::LevelFilter::from_level(tracing::Level::DEBUG)
        );
        let errors = config.errors();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].starts_with("config:") && errors[0].contains("01-bad.toml: "),
            "{errors:?}"
        );
        assert!(!errors[0].contains('\n'), "one line per error: {errors:?}");

        // The main file too: it is not skipped in silence.
        std::fs::write(&main, "[daemon]\nsocket_path = \n").unwrap();
        let errors = Config::load_from(&main).errors();
        assert!(
            errors.iter().any(|e| e.contains("config.toml: ")),
            "{errors:?}"
        );
    }

    #[test]
    fn a_missing_main_file_is_not_an_error() {
        let (_tmp, main, conf_d) = config_dir();
        std::fs::write(
            conf_d.join("01-daemon.toml"),
            "[daemon]\nlog_level = \"debug\"\n",
        )
        .unwrap();
        let config = Config::load_from(&main);
        assert_eq!(config.errors(), Vec::<String>::new());
        assert_eq!(
            config.daemon().log_level,
            tracing_subscriber::filter::LevelFilter::from_level(tracing::Level::DEBUG)
        );
    }

    #[test]
    fn a_composition_that_does_not_deserialize_is_an_error() {
        let (_tmp, main, _conf_d) = config_dir();
        std::fs::write(&main, "[daemon]\nprovider_timeout_secs = \"x\"\n").unwrap();
        assert_eq!(Config::load_from(&main).errors().len(), 1);
    }
}
