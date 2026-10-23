use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::warn;

/// Deep-merge `overlay` onto `base`: matching table keys recurse; everything
/// else (scalars, arrays, and table/non-table type mismatches) is last-wins
/// — `overlay`'s value replaces whatever was at that slot in `base`.
///
/// This is the composition primitive behind conf.d drop-ins (canon
/// `docs/roadmap.md` conf.d convention): a drop-in overriding one key of
/// `[providers.x]` must not clobber the rest of that block, which is exactly
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

// ── Source-knob keys that are ONLY valid inside [providers.<name>.<source>] blocks,
// not at the top-level [providers.<name>] block.
const SOURCE_KNOB_KEYS: &[&str] = &[
    "poll_interval",
    "poll_count",
    "fsevent_patterns",
    "fsevent_abs_paths",
    "fsevent_reinstates",
    "failback_count",
    "failback_interval",
    // Legacy flat keys that have moved to per-source blocks
    "poll_interval_secs",
    "poll_live_count",
    "fsevents_reinstate",
    "failure_reattempts",
    "failure_backoff_interval",
    "poll_secs",
];

/// Where a removed script provider went.
const SCRIPT_REMOVED: &str = "Script providers are removed: write a Source whose data calls \
                              exec(), in [daemon.providers.<p>.sources.<s>]";

/// Which resolution step supplied a socket path
/// (canon singleton.md §"Canonical socket path resolution").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketPathSource {
    ConfigOverride,
    EnvVar,
    Default,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub daemon: DaemonConfig,
    /// `[client]`: what a client reads from the daemon's configuration; today
    /// only `providers` (`[client.providers.<p>] template.<leaf> = "…"`, seeded
    /// as `<p>:template.<leaf>`). A `toml::Value`, so that a `[client]` of the
    /// wrong type still deserializes and the per-file check
    /// (`Config::validate_seeded`) can report it, rather than `Config::load`
    /// falling back to the default configuration.
    #[serde(default = "empty_table")]
    pub client: toml::Value,
    pub lifecycle: LifecycleConfig,
    pub failback: FailbackGlobalConfig,
    /// Raw TOML value for providers. Use accessor methods to get typed views.
    /// Stored as raw toml::Value so we can distinguish scalar provider-level keys
    /// from nested per-source sub-tables.
    #[serde(default)]
    pub providers: HashMap<String, toml::Value>,
    /// The `config.toml` this config was loaded from, whether or not the file
    /// exists — `Config::conf_d_dir_for` gives the other half of the composed
    /// set, so the filename is spelled in one place. `Some` only for a
    /// `Config::load()`; `None` for one built in memory or parsed from a
    /// string, which corresponds to no file at all.
    ///
    /// The daemon seeds the config-seeded facets and watches for changes only when this is
    /// `Some`, so an embedded or test daemon handed a hand-built `Config` reads
    /// nothing from the ambient `~/.config/beachcomber`.
    #[serde(skip)]
    pub config_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    pub socket_path: Option<String>,
    pub log_level: String,
    pub provider_timeout_secs: Option<u64>,
    /// Path to an env file loaded at daemon startup.
    /// Each line is KEY=VALUE (or KEY="VALUE"). Blank lines and #comments are ignored.
    /// These vars are injected into the daemon's environment before any providers execute,
    /// making them available to ${VAR} expansion in HTTP headers and the commands a Source's data runs.
    /// Default: ~/.config/beachcomber/env
    pub env_file: Option<String>,
    /// How often the watchdog checks the scheduler heartbeat. Default: disabled (None).
    /// Example: "30s", "1m".
    pub watchdog_interval: Option<String>,
    /// How long the heartbeat can be stale before the watchdog restarts the scheduler.
    /// Default: 3x watchdog_interval.
    /// Example: "90s", "3m".
    pub watchdog_threshold: Option<String>,
    /// `[daemon.providers.<p>]`: what the daemon holds about a provider:
    /// `cache` (`[daemon.providers.<p>.cache._] decay = N`, seeded as
    /// `<p>:cache._.decay`, and `*:cache` for `<p> = _`) and `sources`
    /// (`[daemon.providers.<p>.sources.<s>]`, built by `crate::shape::build`). A `toml::Value` for
    /// the reason `Config::client` is one; the per-file check is
    /// `Config::validate_seeded`.
    #[serde(default = "empty_table")]
    pub providers: toml::Value,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            daemon: DaemonConfig::default(),
            client: empty_table(),
            lifecycle: LifecycleConfig::default(),
            failback: FailbackGlobalConfig::default(),
            providers: HashMap::new(),
            config_path: None,
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
            log_level: "info".to_string(),
            provider_timeout_secs: Some(10),
            env_file: None,
            watchdog_interval: None,
            watchdog_threshold: None,
            providers: empty_table(),
        }
    }
}

impl DaemonConfig {
    pub fn watchdog_interval_duration(&self) -> Option<Duration> {
        self.watchdog_interval
            .as_ref()
            .and_then(|s| parse_whole_seconds(s))
    }

    pub fn watchdog_threshold_duration(&self) -> Option<Duration> {
        if let Some(ref s) = self.watchdog_threshold {
            parse_whole_seconds(s)
        } else {
            // Default: 3x the watchdog interval
            self.watchdog_interval_duration().map(|d| d * 3)
        }
    }
}

fn default_poll_interval() -> String {
    "60s".to_string()
}

fn default_poll_live_count() -> u32 {
    12
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LifecycleConfig {
    pub idle_shutdown_secs: Option<u64>,
    /// Global default poll interval. Used when no per-source override is set.
    #[serde(default = "default_poll_interval")]
    pub poll_interval: String,
    /// Global default polls in the Active window.
    #[serde(default = "default_poll_live_count")]
    pub poll_live_count: u32,
    /// Global default for `fsevents_reinstate`. `None` means "unset" —
    /// sources fall through to the per-source default declared in `SourceMetadata`.
    /// `Some(v)` is an explicit user override that beats any source default.
    #[serde(default)]
    pub fsevents_reinstate: Option<bool>,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            idle_shutdown_secs: None,
            poll_interval: default_poll_interval(),
            poll_live_count: default_poll_live_count(),
            fsevents_reinstate: None,
        }
    }
}

/// Global failback defaults. Replaces the old `[lifecycle] failure_*` keys.
/// `[lifecycle] failure_reattempts` and `[lifecycle] failure_backoff_interval`
/// are still accepted in `LifecycleConfig` for backward compatibility (they map to
/// `FailbackGlobalConfig` via `Config::failback()`).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FailbackGlobalConfig {
    /// Consecutive failures before suppression. Default 3.
    pub count: u32,
    /// Suppression duration string. Default "1s".
    pub interval: String,
}

impl Default for FailbackGlobalConfig {
    fn default() -> Self {
        Self {
            count: 3,
            interval: "1s".to_string(),
        }
    }
}

impl FailbackGlobalConfig {
    pub fn interval_duration(&self) -> Duration {
        parse_whole_seconds(&self.interval).unwrap_or(Duration::from_secs(1))
    }
}

/// Per-source override block: `[providers.<name>.<source>]`.
/// All keys are optional; missing keys fall through to source defaults,
/// then to [lifecycle]/[failback] globals, then to compile-time defaults.
#[derive(Debug, Clone, Default)]
pub struct SourceOverrideConfig {
    /// Strategy type: "poll", "fsevent", or "fsevent_poll".
    /// For built-ins, must match the Rust declaration if set.
    pub strategy_type: Option<String>,
    /// Scope: "path" or "global". For built-ins, must match Rust declaration if set.
    pub scope: Option<String>,
    /// Whether source is enabled. Default true.
    pub enabled: Option<bool>,
    // poll_* keys
    pub poll_interval: Option<String>,
    pub poll_count: Option<u32>,
    // fsevent_* keys
    pub fsevent_patterns: Option<Vec<String>>,
    pub fsevent_abs_paths: Option<Vec<String>>,
    pub fsevent_reinstates: Option<bool>,
    // failback_* keys
    pub failback_count: Option<u32>,
    pub failback_interval: Option<String>,
}

/// Valid strategy types per-source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrategyKind {
    Poll,
    Fsevent,
    FseventPoll,
}

impl std::str::FromStr for StrategyKind {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "poll" => Ok(Self::Poll),
            "fsevent" => Ok(Self::Fsevent),
            "fsevent_poll" => Ok(Self::FseventPoll),
            _ => Err(()),
        }
    }
}

impl SourceOverrideConfig {
    /// Parse from a `toml::Value::Table`. Returns error if keys are invalid.
    /// `block_name` is used in error messages (e.g. "[providers.git.refs]").
    pub fn from_toml_table(block_name: &str, table: &toml::value::Table) -> Result<Self, String> {
        let mut out = SourceOverrideConfig::default();

        for (key, val) in table {
            match key.as_str() {
                "type" => {
                    let s = val
                        .as_str()
                        .ok_or_else(|| format!("{block_name}: key 'type' must be a string"))?;
                    out.strategy_type = Some(s.to_string());
                }
                "scope" => {
                    let s = val
                        .as_str()
                        .ok_or_else(|| format!("{block_name}: key 'scope' must be a string"))?;
                    out.scope = Some(s.to_string());
                }
                "enabled" => {
                    let b = val
                        .as_bool()
                        .ok_or_else(|| format!("{block_name}: key 'enabled' must be a boolean"))?;
                    out.enabled = Some(b);
                }
                "poll_interval" => {
                    let s = val.as_str().ok_or_else(|| {
                        format!("{block_name}: key 'poll_interval' must be a duration string")
                    })?;
                    out.poll_interval = Some(s.to_string());
                }
                "poll_count" => {
                    let n = val.as_integer().ok_or_else(|| {
                        format!("{block_name}: key 'poll_count' must be an integer")
                    })?;
                    out.poll_count = Some(n as u32);
                }
                "fsevent_patterns" => {
                    let arr = val.as_array().ok_or_else(|| {
                        format!("{block_name}: key 'fsevent_patterns' must be an array")
                    })?;
                    let patterns: Result<Vec<String>, _> = arr
                        .iter()
                        .map(|v| {
                            v.as_str().map(|s| s.to_string()).ok_or_else(|| {
                                format!("{block_name}: fsevent_patterns elements must be strings")
                            })
                        })
                        .collect();
                    out.fsevent_patterns = Some(patterns?);
                }
                "fsevent_abs_paths" => {
                    let arr = val.as_array().ok_or_else(|| {
                        format!("{block_name}: key 'fsevent_abs_paths' must be an array")
                    })?;
                    let paths: Result<Vec<String>, _> = arr
                        .iter()
                        .map(|v| {
                            v.as_str().map(|s| s.to_string()).ok_or_else(|| {
                                format!("{block_name}: fsevent_abs_paths elements must be strings")
                            })
                        })
                        .collect();
                    out.fsevent_abs_paths = Some(paths?);
                }
                "fsevent_reinstates" => {
                    let b = val.as_bool().ok_or_else(|| {
                        format!("{block_name}: key 'fsevent_reinstates' must be a boolean")
                    })?;
                    out.fsevent_reinstates = Some(b);
                }
                "failback_count" => {
                    let n = val.as_integer().ok_or_else(|| {
                        format!("{block_name}: key 'failback_count' must be an integer")
                    })?;
                    out.failback_count = Some(n as u32);
                }
                "failback_interval" => {
                    let s = val.as_str().ok_or_else(|| {
                        format!("{block_name}: key 'failback_interval' must be a duration string")
                    })?;
                    out.failback_interval = Some(s.to_string());
                }
                other => {
                    return Err(format!(
                        "{block_name}: unknown key '{other}'. Valid per-source keys are: \
                         type, scope, enabled, poll_interval, poll_count, fsevent_patterns, \
                         fsevent_abs_paths, fsevent_reinstates, \
                         failback_count, failback_interval"
                    ));
                }
            }
        }

        Ok(out)
    }

    /// Validate that the declared keys are consistent with the given strategy type.
    /// Called only when `type` is set and parseable.
    pub fn validate_strategy_keys(&self, block_name: &str) -> Result<(), String> {
        let kind = match self.strategy_type.as_deref() {
            Some(t) => match t.parse::<StrategyKind>() {
                Ok(k) => k,
                Err(()) => {
                    return Err(format!(
                        "{block_name}: unknown strategy type '{}'. \
                         Valid values: \"poll\", \"fsevent\", \"fsevent_poll\"",
                        t
                    ));
                }
            },
            None => return Ok(()), // No type declared; skip strategy validation.
        };

        // poll_* keys only valid for poll and fsevent_poll
        if (self.poll_interval.is_some() || self.poll_count.is_some())
            && kind == StrategyKind::Fsevent
        {
            return Err(format!(
                "{block_name}: keys poll_interval/poll_count are not valid for \
                     type = \"fsevent\". Use type = \"poll\" or \"fsevent_poll\"."
            ));
        }

        // fsevent_* keys only valid for fsevent and fsevent_poll
        if (self.fsevent_patterns.is_some()
            || self.fsevent_abs_paths.is_some()
            || self.fsevent_reinstates.is_some())
            && kind == StrategyKind::Poll
        {
            return Err(format!(
                "{block_name}: keys fsevent_patterns/fsevent_abs_paths/\
                     fsevent_reinstates are not valid for type = \"poll\". \
                     Use type = \"fsevent\" or \"fsevent_poll\"."
            ));
        }

        Ok(())
    }
}

/// Per-source config for external backends (http / library).
///
/// Used when a provider is declared with `backend = "http"` (or `"library"`)
/// and each source lives in its own `[providers.<name>.<source>]` sub-table.
///
/// Accepts both declaration keys (`url`, `fields`, …) and all knob keys
/// (`poll_interval`, `fsevent_patterns`, `failback_count`, …).
#[derive(Debug, Clone, Default)]
pub struct ExternalSourceConfig {
    /// Source name (filled by the caller, not parsed from the table itself).
    pub name: String,
    /// Strategy type: "poll", "fsevent", or "fsevent_poll".
    pub strategy_type: Option<String>,
    /// Scope: "global" or "path".
    pub scope: Option<String>,
    /// Whether the source is enabled. Default true.
    pub enabled: Option<bool>,

    // ── HTTP-backend declaration keys ─────────────────────────────────────────
    pub url: Option<String>,
    pub method: Option<String>,
    /// HTTP headers as key→value map.
    pub headers: Option<HashMap<String, String>>,
    pub body: Option<String>,
    pub extract: Option<String>,
    pub default_timeout: Option<String>,

    // ── Field declarations ────────────────────────────────────────────────────
    pub fields: Option<Vec<ExternalFieldDecl>>,

    // ── Knob overrides (same vocabulary as SourceOverrideConfig) ─────────────
    pub poll_interval: Option<String>,
    pub poll_count: Option<u32>,
    pub fsevent_patterns: Option<Vec<String>>,
    pub fsevent_abs_paths: Option<Vec<String>>,
    pub fsevent_reinstates: Option<bool>,
    pub failback_count: Option<u32>,
    pub failback_interval: Option<String>,
}

/// A field declaration inside an external source config block.
///
/// ```toml
/// fields = [{ name = "temp", type = "float" }]
/// ```
#[derive(Debug, Clone, Default)]
pub struct ExternalFieldDecl {
    pub name: String,
    pub field_type: String,
}

/// One `[providers.<name>]` block with `backend = "library"`, paired with `library_path`
/// and any per-source override tables.
pub type LibraryProviderBlocks = Vec<(String, String, Vec<ExternalSourceConfig>)>;

/// One `[providers.<name>]` block with `backend = "http"`, paired with the optional
/// `default_timeout` and parsed sources.
pub type HttpProviderBlocks = Vec<(String, Option<String>, Vec<ExternalSourceConfig>)>;

impl ExternalSourceConfig {
    /// Parse from a TOML table. `block_name` is used in error messages.
    pub fn from_toml_table(block_name: &str, table: &toml::value::Table) -> Result<Self, String> {
        let mut out = ExternalSourceConfig::default();

        // ── headers sub-table ─────────────────────────────────────────────────
        // headers may appear as a sub-table; we collect it first before the
        // scalar-key loop which skips tables.
        if let Some(headers_val) = table.get("headers")
            && let Some(headers_table) = headers_val.as_table()
        {
            let mut map = HashMap::new();
            for (k, v) in headers_table {
                let s = v
                    .as_str()
                    .ok_or_else(|| format!("{block_name}: headers.{k} must be a string"))?;
                map.insert(k.clone(), s.to_string());
            }
            out.headers = Some(map);
        }

        for (key, val) in table {
            match key.as_str() {
                "type" => {
                    out.strategy_type = Some(
                        val.as_str()
                            .ok_or_else(|| format!("{block_name}: 'type' must be a string"))?
                            .to_string(),
                    );
                }
                "scope" => {
                    out.scope = Some(
                        val.as_str()
                            .ok_or_else(|| format!("{block_name}: 'scope' must be a string"))?
                            .to_string(),
                    );
                }
                "enabled" => {
                    out.enabled = Some(
                        val.as_bool()
                            .ok_or_else(|| format!("{block_name}: 'enabled' must be a boolean"))?,
                    );
                }
                // HTTP
                "url" => {
                    out.url = Some(
                        val.as_str()
                            .ok_or_else(|| format!("{block_name}: 'url' must be a string"))?
                            .to_string(),
                    );
                }
                "method" => {
                    out.method = Some(
                        val.as_str()
                            .ok_or_else(|| format!("{block_name}: 'method' must be a string"))?
                            .to_string(),
                    );
                }
                "headers" => {
                    // Already handled above; skip here.
                }
                "body" => {
                    out.body = Some(
                        val.as_str()
                            .ok_or_else(|| format!("{block_name}: 'body' must be a string"))?
                            .to_string(),
                    );
                }
                "extract" => {
                    out.extract = Some(
                        val.as_str()
                            .ok_or_else(|| format!("{block_name}: 'extract' must be a string"))?
                            .to_string(),
                    );
                }
                "default_timeout" => {
                    out.default_timeout = Some(
                        val.as_str()
                            .ok_or_else(|| {
                                format!("{block_name}: 'default_timeout' must be a duration string")
                            })?
                            .to_string(),
                    );
                }
                // Fields array
                "fields" => {
                    let arr = val
                        .as_array()
                        .ok_or_else(|| format!("{block_name}: 'fields' must be an array"))?;
                    let mut field_decls = Vec::new();
                    for (i, item) in arr.iter().enumerate() {
                        let tbl = item
                            .as_table()
                            .ok_or_else(|| format!("{block_name}: fields[{i}] must be a table"))?;
                        let name = tbl
                            .get("name")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| {
                                format!("{block_name}: fields[{i}].name is required and must be a string")
                            })?
                            .to_string();
                        let field_type = tbl
                            .get("type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("string")
                            .to_string();
                        field_decls.push(ExternalFieldDecl { name, field_type });
                    }
                    out.fields = Some(field_decls);
                }
                // Knob keys
                "poll_interval" => {
                    out.poll_interval = Some(
                        val.as_str()
                            .ok_or_else(|| {
                                format!("{block_name}: 'poll_interval' must be a duration string")
                            })?
                            .to_string(),
                    );
                }
                "poll_count" => {
                    out.poll_count =
                        Some(val.as_integer().ok_or_else(|| {
                            format!("{block_name}: 'poll_count' must be an integer")
                        })? as u32);
                }
                "fsevent_patterns" => {
                    let arr = val.as_array().ok_or_else(|| {
                        format!("{block_name}: 'fsevent_patterns' must be an array")
                    })?;
                    let v: Result<Vec<String>, _> = arr
                        .iter()
                        .map(|v| {
                            v.as_str().map(|s| s.to_string()).ok_or_else(|| {
                                format!("{block_name}: fsevent_patterns elements must be strings")
                            })
                        })
                        .collect();
                    out.fsevent_patterns = Some(v?);
                }
                "fsevent_abs_paths" => {
                    let arr = val.as_array().ok_or_else(|| {
                        format!("{block_name}: 'fsevent_abs_paths' must be an array")
                    })?;
                    let v: Result<Vec<String>, _> = arr
                        .iter()
                        .map(|v| {
                            v.as_str().map(|s| s.to_string()).ok_or_else(|| {
                                format!("{block_name}: fsevent_abs_paths elements must be strings")
                            })
                        })
                        .collect();
                    out.fsevent_abs_paths = Some(v?);
                }
                "fsevent_reinstates" => {
                    out.fsevent_reinstates = Some(val.as_bool().ok_or_else(|| {
                        format!("{block_name}: 'fsevent_reinstates' must be a boolean")
                    })?);
                }
                "failback_count" => {
                    out.failback_count = Some(val.as_integer().ok_or_else(|| {
                        format!("{block_name}: 'failback_count' must be an integer")
                    })? as u32);
                }
                "failback_interval" => {
                    out.failback_interval = Some(
                        val.as_str()
                            .ok_or_else(|| {
                                format!(
                                    "{block_name}: 'failback_interval' must be a duration string"
                                )
                            })?
                            .to_string(),
                    );
                }
                other => {
                    return Err(format!(
                        "{block_name}: unknown key '{other}'. \
                         Valid external-source keys are: type, scope, enabled, \
                         url, method, headers, body, extract, default_timeout, fields, \
                         poll_interval, poll_count, fsevent_patterns, fsevent_abs_paths, \
                         fsevent_reinstates, failback_count, failback_interval"
                    ));
                }
            }
        }

        Ok(out)
    }
}

/// A library field declaration: either a bare type string or a table with
/// the type.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum FieldSpec {
    Simple(String),
    Detailed {
        #[serde(rename = "type")]
        field_type: String,
    },
}

impl FieldSpec {
    pub fn field_type(&self) -> &str {
        match self {
            FieldSpec::Simple(t) => t,
            FieldSpec::Detailed { field_type } => field_type,
        }
    }
}

/// Configuration for a library/http provider extracted from the raw providers map.
/// This preserves backward compatibility for Phase 4 which will restructure external backends.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct FlatProviderConfig {
    #[serde(rename = "type")]
    pub provider_type: Option<String>,
    pub invalidation: Option<FlatInvalidation>,
    pub fields: Option<HashMap<String, FieldSpec>>,
    pub scope: Option<String>,
    pub enabled: Option<bool>,
    pub poll_secs: Option<u64>,
    // HTTP provider fields (used when type = "http")
    pub url: Option<String>,
    pub method: Option<String>,
    pub headers: Option<HashMap<String, String>>,
    pub body: Option<String>,
    pub extract: Option<String>,
    // Library provider fields (used when type = "library")
    pub library_path: Option<String>,
    // New configurable backoff fields (override lifecycle defaults)
    pub failure_reattempts: Option<u32>,
    pub failure_backoff_interval: Option<String>,
    // New lifecycle config fields (Task 8)
    pub poll_interval: Option<String>,
    pub poll_live_count: Option<u32>,
    pub fsevents_reinstate: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct HttpProviderConfig {
    #[serde(rename = "type")]
    pub provider_type: Option<String>,
    pub url: String,
    pub method: Option<String>,
    pub headers: Option<HashMap<String, String>>,
    pub body: Option<String>,
    pub extract: Option<String>,
    pub invalidation: Option<FlatInvalidation>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct FlatInvalidation {
    pub poll: Option<String>,
    pub watch: Option<Vec<String>>,
}

impl Config {
    /// Returns true if a provider is explicitly disabled in config.
    pub fn is_provider_disabled(&self, name: &str) -> bool {
        self.providers
            .get(name)
            .and_then(|v| v.get("enabled"))
            .and_then(|v| v.as_bool())
            .map(|e| !e)
            .unwrap_or(false)
    }

    /// Parse the per-source override config for (provider, source).
    /// Returns None if no block exists.
    /// Returns Err if the block exists but is malformed.
    pub fn source_override(
        &self,
        provider_name: &str,
        source_name: &str,
    ) -> Result<Option<SourceOverrideConfig>, String> {
        let provider_val = match self.providers.get(provider_name) {
            Some(v) => v,
            None => return Ok(None),
        };
        let source_val = match provider_val.get(source_name) {
            Some(v) => v,
            None => return Ok(None),
        };
        let table = match source_val.as_table() {
            Some(t) => t,
            None => {
                return Err(format!(
                    "[providers.{provider_name}.{source_name}] must be a table"
                ));
            }
        };
        let block_name = format!("[providers.{provider_name}.{source_name}]");
        let cfg = SourceOverrideConfig::from_toml_table(&block_name, table)?;
        cfg.validate_strategy_keys(&block_name)?;
        Ok(Some(cfg))
    }

    // ── Resolution methods ────────────────────────────────────────────────────
    //
    // Resolution order per spec §"Resolution order":
    //   1. [providers.<provider>.<source>] block (user override)
    //   2. Source's declared default in SourceMetadata (passed as `source_default`)
    //   3. [lifecycle] / [failback] global blocks
    //   4. Compile-time defaults

    /// Resolve failure reattempts for a (provider, source) pair.
    pub fn resolve_failure_reattempts(&self, provider_name: &str) -> u32 {
        self.resolve_failure_reattempts_for_source(provider_name, None, None)
    }

    /// Resolve failure reattempts for a (provider, source) pair with optional source override.
    pub fn resolve_failure_reattempts_for_source(
        &self,
        provider_name: &str,
        source_name: Option<&str>,
        source_default: Option<u32>,
    ) -> u32 {
        // 1. Per-source override
        if let Some(src) = source_name
            && let Ok(Some(cfg)) = self.source_override(provider_name, src)
            && let Some(v) = cfg.failback_count
        {
            return v;
        }
        // 2. Source-declared default
        if let Some(d) = source_default {
            return d;
        }
        // 3. [failback] global
        // 4. Compile-time default
        self.failback.count
    }

    /// Resolve failure backoff interval for a (provider, source) pair.
    pub fn resolve_failure_backoff_interval(&self, provider_name: &str) -> Duration {
        self.resolve_failure_backoff_for_source(provider_name, None, None)
    }

    /// Resolve failure backoff interval for a (provider, source) pair with optional source override.
    pub fn resolve_failure_backoff_for_source(
        &self,
        provider_name: &str,
        source_name: Option<&str>,
        source_default: Option<Duration>,
    ) -> Duration {
        // 1. Per-source override
        if let Some(src) = source_name
            && let Ok(Some(cfg)) = self.source_override(provider_name, src)
            && let Some(ref s) = cfg.failback_interval
            && let Some(d) = parse_whole_seconds(s)
        {
            return d;
        }
        // 2. Source-declared default
        if let Some(d) = source_default {
            return d;
        }
        // 3. [failback] global then compile-time default
        self.failback.interval_duration()
    }

    /// Resolve poll interval for a (provider, source) pair.
    pub fn resolve_poll_interval(&self, provider_name: &str) -> Duration {
        self.resolve_poll_interval_for_source(provider_name, None, None)
    }

    /// Resolve poll interval for a (provider, source) pair with optional source override.
    pub fn resolve_poll_interval_for_source(
        &self,
        provider_name: &str,
        source_name: Option<&str>,
        source_default: Option<Duration>,
    ) -> Duration {
        // 1. Per-source override
        if let Some(src) = source_name
            && let Ok(Some(cfg)) = self.source_override(provider_name, src)
            && let Some(ref s) = cfg.poll_interval
            && let Some(d) = parse_whole_seconds(s)
        {
            return d;
        }
        // 2. Source-declared default
        if let Some(d) = source_default {
            return d;
        }
        // 3. [lifecycle] global then compile-time default
        parse_whole_seconds(&self.lifecycle.poll_interval).unwrap_or(Duration::from_secs(60))
    }

    /// Resolve the polls in the Active window for a (provider, source) pair.
    pub fn resolve_poll_live_count(&self, provider_name: &str) -> u32 {
        self.resolve_poll_live_count_for_source(provider_name, None, None)
    }

    /// Resolve the polls in the Active window for a (provider, source) pair with optional source override.
    pub fn resolve_poll_live_count_for_source(
        &self,
        provider_name: &str,
        source_name: Option<&str>,
        source_default: Option<u32>,
    ) -> u32 {
        // 1. Per-source override
        if let Some(src) = source_name
            && let Ok(Some(cfg)) = self.source_override(provider_name, src)
            && let Some(v) = cfg.poll_count
        {
            return v;
        }
        // 2. Source-declared default
        if let Some(d) = source_default {
            return d;
        }
        // 3. [lifecycle] global then compile-time default
        self.lifecycle.poll_live_count
    }

    /// Resolve the effective `fsevents_reinstate` flag for a (provider, source) pair.
    ///
    /// Priority (first match wins):
    /// 1. Per-source config: `[providers.<name>.<source>] fsevent_reinstates = <bool>`.
    /// 2. Global lifecycle override: `[lifecycle] fsevents_reinstate = <bool>`.
    /// 3. Source-declared default via `SourceMetadata::fsevents_reinstate`.
    pub fn resolve_fsevents_reinstate(&self, provider_name: &str, provider_default: bool) -> bool {
        self.resolve_fsevents_reinstate_for_source(provider_name, None, provider_default)
    }

    /// Resolve `fsevents_reinstate` for a specific (provider, source) pair.
    pub fn resolve_fsevents_reinstate_for_source(
        &self,
        provider_name: &str,
        source_name: Option<&str>,
        source_default: bool,
    ) -> bool {
        // 1. Per-source override
        if let Some(src) = source_name
            && let Ok(Some(cfg)) = self.source_override(provider_name, src)
            && let Some(v) = cfg.fsevent_reinstates
        {
            return v;
        }
        // 2. Global lifecycle override
        if let Some(v) = self.lifecycle.fsevents_reinstate {
            return v;
        }
        // 3. Source-declared default
        source_default
    }

    // ── External backend provider extraction ─────────────────────────────────
    //
    // Two families of extraction:
    //
    // 1. Single-source: provider-level block carries all config. Used when
    //    there is NO `backend` key. `library_providers()`, `http_providers()`.
    //
    // 2. Multi-source: provider-level block has `backend = "..."` key; each
    //    source lives in a per-source sub-table. The provider-level block may
    //    also carry `library_path` (library) or `default_timeout` (http).
    //    `multi_library_providers()`, `multi_http_providers()`.

    fn as_external_provider_config(val: &toml::Value) -> Option<FlatProviderConfig> {
        // For external backends (library/http), deserialize the full provider-level
        // table as FlatProviderConfig. Sub-table entries like `fields` and `invalidation`
        // are part of the external backend config shape (Phase 4 will restructure these).
        let table = val.as_table()?.clone();
        table.try_into().ok()
    }

    /// Return the `backend` value for a provider's top-level block, if present.
    fn backend_for(val: &toml::Value) -> Option<&str> {
        val.get("backend").and_then(|v| v.as_str())
    }

    pub fn library_providers(&self) -> Vec<(String, FlatProviderConfig)> {
        self.providers
            .iter()
            .filter_map(|(name, val)| {
                // Skip Phase 4 multi-source style (has `backend` key).
                if Self::backend_for(val).is_some() {
                    return None;
                }
                let cfg = Self::as_external_provider_config(val)?;
                if cfg.provider_type.as_deref() == Some("library") {
                    Some((name.clone(), cfg))
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn http_providers(&self) -> Vec<(String, HttpProviderConfig)> {
        self.providers
            .iter()
            .filter_map(|(name, val)| {
                // Skip Phase 4 multi-source style (has `backend` key).
                if Self::backend_for(val).is_some() {
                    return None;
                }
                let cfg = Self::as_external_provider_config(val)?;
                if cfg.provider_type.as_deref() != Some("http") {
                    return None;
                }
                Some((
                    name.clone(),
                    HttpProviderConfig {
                        provider_type: cfg.provider_type,
                        url: cfg.url.unwrap_or_default(),
                        method: cfg.method,
                        headers: cfg.headers,
                        body: cfg.body,
                        extract: cfg.extract,
                        invalidation: cfg.invalidation,
                    },
                ))
            })
            .collect()
    }

    // ── Phase 4 multi-source external backend extraction ─────────────────────

    /// Parse the per-source sub-tables for a provider declared with `backend = "..."`.
    /// Returns `Err` if any sub-table is malformed, `Ok(vec)` otherwise (may be empty).
    fn parse_external_sources(
        prov_name: &str,
        prov_val: &toml::Value,
    ) -> Result<Vec<ExternalSourceConfig>, String> {
        let table = match prov_val.as_table() {
            Some(t) => t,
            None => return Ok(vec![]),
        };
        let mut sources = Vec::new();
        for (sub_key, sub_val) in table {
            // Skip scalar provider-level keys; only process sub-tables.
            let sub_table = match sub_val.as_table() {
                Some(t) => t,
                None => continue,
            };
            // Skip known provider-level-only sub-table names.
            if matches!(sub_key.as_str(), "invalidation" | "fields") {
                // "invalidation"/"fields" appear in legacy flat shape; in multi-source they
                // live inside per-source blocks instead. Skip both at provider level.
                continue;
            }
            // Not a source either; `validate_providers` owns the error for it.
            if sub_key == "virtual" {
                continue;
            }
            let block_name = format!("[providers.{prov_name}.{sub_key}]");
            let mut src = ExternalSourceConfig::from_toml_table(&block_name, sub_table)?;
            src.name = sub_key.clone();
            sources.push(src);
        }
        Ok(sources)
    }

    /// Multi-source library providers: `backend = "library"` + `library_path`.
    /// Source list comes from the library's C ABI; user TOML sub-tables only
    /// override knobs, not declare sources. Returns `(name, library_path, source_overrides)`.
    pub fn multi_library_providers(&self) -> Result<LibraryProviderBlocks, String> {
        let mut result = Vec::new();
        for (name, val) in &self.providers {
            if Self::backend_for(val) != Some("library") {
                continue;
            }
            let library_path = val
                .get("library_path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    format!(
                        "provider '{}' (backend = library): missing required key 'library_path'",
                        name
                    )
                })?
                .to_string();
            let sources = Self::parse_external_sources(name, val)?;
            result.push((name.clone(), library_path, sources));
        }
        Ok(result)
    }

    /// Multi-source HTTP providers: `backend = "http"` + per-source sub-tables.
    /// Each source sub-table must contain `url`. Returns `(name, default_timeout, sources)`.
    pub fn multi_http_providers(&self) -> Result<HttpProviderBlocks, String> {
        let mut result = Vec::new();
        for (name, val) in &self.providers {
            if Self::backend_for(val) != Some("http") {
                continue;
            }
            let default_timeout = val
                .get("default_timeout")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let sources = Self::parse_external_sources(name, val)?;
            // Validate: each source must have a url.
            for src in &sources {
                if src.url.is_none() {
                    return Err(format!(
                        "provider '{}' source '{}' (backend = http): \
                         missing required key 'url'",
                        name, src.name
                    ));
                }
            }
            result.push((name.clone(), default_timeout, sources));
        }
        Ok(result)
    }

    // ── Startup validation ────────────────────────────────────────────────────

    /// Validate provider config blocks. Returns (warnings, errors).
    /// - Unknown provider names on built-in providers → error.
    /// - Known provider, unknown source name → warn (cross-platform-config support).
    /// - Old flat source-knob keys at provider level → error.
    /// - Invalid strategy keys for declared type → error.
    ///
    /// `known_providers` is the list of registered provider names.
    /// `known_sources` maps provider_name → list of source names.
    pub fn validate_providers(
        &self,
        known_providers: &[String],
        known_sources: &HashMap<String, Vec<String>>,
    ) -> (Vec<String>, Vec<String>) {
        let mut warnings = Vec::new();
        let mut errors = Vec::new();

        for (prov_name, prov_val) in &self.providers {
            let table = match prov_val.as_table() {
                Some(t) => t,
                None => continue,
            };

            // A colon-named table is the removed facet-table form;
            // `validate_seeded_tables` reports it and takes the whole table
            // with it, so it is never read as a source block for its provider.
            if prov_name.contains(':') {
                continue;
            }

            // `path` was the path-expression setting: a client-side expression
            // that computed the entry a query read. A query addresses the
            // caller's directory, `--path`, or `--global` now, so a config
            // still carrying the key is told where the address went. A table
            // under this name is a source block and falls through to the
            // per-source loop below.
            if table.get("path").is_some_and(|v| !v.is_table()) {
                errors.push(format!(
                    "[providers.{prov_name}]: path is not a provider setting: \
                     a query addresses the caller's directory, --path, or --global"
                ));
            }

            // A script provider is a Source whose data calls exec(): the
            // discriminators and the keys only it read are errors.
            let removed = [
                ("type", "type = \"script\""),
                ("backend", "backend = \"script\""),
            ]
            .iter()
            .filter(|(key, _)| table.get(*key).and_then(|v| v.as_str()) == Some("script"))
            .map(|(key, shown)| {
                format!("[providers.{prov_name}] {shown}: unknown value for '{key}'")
            })
            .chain(
                table
                    .contains_key("command")
                    .then(|| format!("[providers.{prov_name}]: unknown key 'command'")),
            )
            .chain(
                // A table named `output` is a Source block, not the key.
                table
                    .get("output")
                    .is_some_and(|v| !v.is_table())
                    .then(|| format!("[providers.{prov_name}]: unknown key 'output'")),
            )
            .collect::<Vec<_>>();
            if !removed.is_empty() {
                errors.extend(
                    removed
                        .into_iter()
                        .map(|e| format!("{e}. {SCRIPT_REMOVED}")),
                );
                continue;
            }

            // A flat provider's `fields` table maps a name to a type, bare or
            // as `{ type = … }`; a per-field `scope` is not a key.
            if let Some(fields) = table.get("fields").and_then(|f| f.as_table()) {
                for (field, spec) in fields {
                    if let Some(spec) = spec.as_table() {
                        for key in spec.keys().filter(|k| *k != "type") {
                            errors.push(format!(
                                "[providers.{prov_name}] fields.{field}: unknown key '{key}' \
                                 (a field holds only its type)"
                            ));
                        }
                    }
                }
            }

            // Check for old flat source-knob keys at the provider level.
            // These are rejected for ALL providers (built-in and external).
            // Exception: library/http providers still use some of these
            // keys (failure_reattempts, failure_backoff_interval, poll_interval,
            // poll_live_count, fsevents_reinstate, poll_secs) at provider level
            // until Phase 4. We detect if this looks like an external backend.
            let is_external_backend = table
                .get("type")
                .and_then(|v| v.as_str())
                .map(|t| matches!(t, "library" | "http"))
                .unwrap_or(false)
                || table
                    .get("backend")
                    .and_then(|v| v.as_str())
                    .map(|b| matches!(b, "library" | "http"))
                    .unwrap_or(false)
                || table.get("url").is_some()
                || table.get("library_path").is_some();

            if !is_external_backend {
                // For non-external (built-in or declared) providers, reject old flat
                // source-knob keys. These indicate the old schema.
                for key in SOURCE_KNOB_KEYS {
                    if table.contains_key(*key) {
                        errors.push(format!(
                            "[providers.{prov_name}] contains source-knob key '{key}'. \
                             In the new schema, source-knob keys belong in a per-source block: \
                             [providers.{prov_name}.<source_name>]. \
                             See the configuration reference for the new per-source schema with \
                             poll_*, fsevent_*, and failback_* prefixes."
                        ));
                    }
                }
            }

            // Validate per-source sub-table blocks.
            for (sub_key, sub_val) in table {
                if !sub_val.is_table() {
                    // Scalar key; already handled above or is a provider-level key.
                    continue;
                }

                // The 0.9 home of a provider's templates. Templates are data
                // the daemon seeds from `[client.providers.<p>] template`
                // now, so this block is unrecognised — and the one
                // unrecognised block whose error says where its contents
                // went, because a config written against 0.9 will carry it.
                if sub_key == "virtual" {
                    errors.push(format!(
                        "unrecognised block [providers.{prov_name}.virtual]: \
                         templates live in [client.providers.{prov_name}] template.<leaf>"
                    ));
                    continue;
                }

                // Legacy flat external-backend shape (`type = "http"` or `"library"`, no
                // `backend` key) uses `fields` and `invalidation` as provider-level sub-tables directly
                // (see FlatProviderConfig::{fields, invalidation}) — they are not
                // per-source blocks. Skip them here so they aren't flagged as
                // unrecognized source blocks, mirroring parse_external_sources' skip
                // logic. Scoped to the no-`backend`-key case so Phase 4 `backend = "..."`
                // providers (which declare real sources, not these keys) are unaffected.
                if is_external_backend
                    && table.get("backend").is_none()
                    && matches!(sub_key.as_str(), "fields" | "invalidation")
                {
                    continue;
                }

                // sub_key is a potential source name.
                let sub_table = sub_val.as_table().unwrap();
                let block_name = format!("[providers.{prov_name}.{sub_key}]");

                if is_external_backend {
                    // Phase 4 multi-source: validate using ExternalSourceConfig parser
                    // (which accepts backend-specific declaration keys).
                    if let Err(e) = ExternalSourceConfig::from_toml_table(&block_name, sub_table) {
                        errors.push(e);
                    }
                    // External backend sources are declared in TOML, not in built-ins;
                    // no warn-and-skip for unknown sources needed.
                    continue;
                }

                // Parse and validate the source override block (built-in provider path).
                match SourceOverrideConfig::from_toml_table(&block_name, sub_table) {
                    Err(e) => {
                        errors.push(e);
                        continue;
                    }
                    Ok(cfg) => {
                        if let Err(e) = cfg.validate_strategy_keys(&block_name) {
                            errors.push(e);
                            continue;
                        }
                    }
                }

                // Check if the provider is a known (built-in) provider.
                let is_known_builtin = known_providers.contains(prov_name);
                if is_known_builtin
                    && let Some(sources) = known_sources.get(prov_name)
                    && !sources.contains(sub_key)
                {
                    warnings.push(format!(
                        "config block {block_name} does not match any registered \
                                 source for provider '{prov_name}' \
                                 (registered: {}) — skipping. \
                                 If this is a typo, fix it; if it's a platform-conditional \
                                 source, this block is inert on this platform.",
                        sources.join(", ")
                    ));
                }
            }
        }

        (warnings, errors)
    }

    pub fn load() -> Self {
        let main_content = Self::config_path_if_exists()
            .and_then(|p| std::fs::read_to_string(&p).ok())
            .unwrap_or_default();
        for warning in detect_deprecated_keys(&main_content) {
            warn!("{}", warning);
        }
        let config_path = Self::config_path();
        let mut config = Self::load_composed(
            &main_content,
            &Self::defaults_dir_for(&config_path),
            &Self::conf_d_dir_for(&config_path),
        );
        // Recorded even when no file exists there: it is where this config
        // resolved to, and a conf.d-only installation is legitimate.
        config.config_path = Some(config_path);
        config
    }

    /// Parse TOML config text into a `Config`, surfacing the parse error
    /// instead of silently falling back to defaults like `load()` does. Used
    /// by the daemon's config self-watch (`src/daemon/config_watch.rs`) to
    /// validate a changed config file before restarting into it.
    pub fn parse_str(content: &str) -> Result<Self, String> {
        toml::from_str(content).map_err(|e| e.to_string())
    }

    /// Compose every `defaults/*.toml` in `defaults_dir`, then `main_content`,
    /// then every `conf.d/*.toml` drop-in found in `conf_d_dir`, each directory
    /// in lexical filename order, each layer deep-merged onto the accumulated
    /// value — then deserialize the result. Mirrors `load()`'s lenient startup
    /// posture: an unparseable file is skipped with a `tracing::warn!` naming
    /// the file and error, the rest still compose. A missing/unparseable
    /// `main_content` composes as an empty table, same as `load()` does for an
    /// absent main config file.
    ///
    /// Kept separate from `load()` so tests can exercise composition against
    /// an explicit temp directory without mutating XDG env vars.
    fn load_composed(main_content: &str, defaults_dir: &Path, conf_d_dir: &Path) -> Self {
        let composed = Self::compose_value(main_content, defaults_dir, conf_d_dir);
        Config::deserialize(composed).unwrap_or_default()
    }

    /// Value-level composition shared by `load_composed` and `parse_composed`.
    /// Skips unparseable files with a warning (the lenient half of the two
    /// postures — `parse_composed` is the strict half). The main file's content
    /// arrives as text rather than a path, so the defaults layer is merged
    /// first, then the main content, then conf.d — the order `layers` lists.
    fn compose_value(main_content: &str, defaults_dir: &Path, conf_d_dir: &Path) -> toml::Value {
        let mut main: toml::Value =
            toml::from_str(main_content).unwrap_or_else(|_| toml::Value::Table(Default::default()));
        if main.as_table().is_none() {
            main = toml::Value::Table(Default::default());
        }
        let mut base = toml::Value::Table(Default::default());
        for file in Self::conf_d_files(defaults_dir) {
            if let Some(overlay) = Self::read_layer(&file) {
                deep_merge_toml(&mut base, overlay);
            }
        }
        deep_merge_toml(&mut base, main);
        for file in Self::conf_d_files(conf_d_dir) {
            if let Some(overlay) = Self::read_layer(&file) {
                deep_merge_toml(&mut base, overlay);
            }
        }
        base
    }

    /// The configuration files that compose, in composition order — every
    /// `defaults/*.toml` (when `defaults` is given), then the main config file
    /// (when `config_path` is given and readable), then every `conf.d/*.toml`,
    /// each directory in lexical filename order — each parsed on its own. A
    /// file that cannot be read or does not parse as a table is skipped with a
    /// warning, the lenient posture `compose_value` folds over.
    ///
    /// Settings compose by deep merge, but facet tables are seeded per layer so
    /// each record keeps the label of the file that set it — which is only
    /// possible while the layers are still separable.
    pub fn layers(
        defaults: Option<&Path>,
        config_path: Option<&Path>,
        conf_d: &Path,
    ) -> Vec<(PathBuf, toml::Value)> {
        let mut files: Vec<PathBuf> = Vec::new();
        if let Some(d) = defaults {
            files.extend(Self::conf_d_files(d));
        }
        if let Some(p) = config_path {
            files.push(p.to_path_buf());
        }
        files.extend(Self::conf_d_files(conf_d));
        files
            .into_iter()
            .filter_map(|file| Self::read_layer(&file).map(|v| (file, v)))
            .collect()
    }

    /// One layer of `layers`: the file parsed as a TOML table, or `None` —
    /// silently for a file that does not exist, with a warning for one that
    /// cannot be read, does not parse, or is not a table.
    fn read_layer(file: &Path) -> Option<toml::Value> {
        let content = match std::fs::read_to_string(file) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                warn!("config: could not read {}: {e}; skipping", file.display());
                return None;
            }
        };
        match toml::from_str::<toml::Value>(&content) {
            Ok(v) if v.is_table() => Some(v),
            Ok(_) => {
                warn!(
                    "config: {} top-level value is not a table; skipping",
                    file.display()
                );
                None
            }
            Err(e) => {
                warn!("config: {} failed to parse ({e}); skipping", file.display());
                None
            }
        }
    }

    /// The errors in the files this configuration composes from, in the two
    /// roots the daemon seeds from (`crate::shape::seeded::read`): checked per
    /// file because seeding is per file, so a rule broken in one file is not
    /// hidden by another file's value winning the merge. Empty for a `Config`
    /// built in memory, which corresponds to no file and seeds nothing.
    pub fn validate_seeded(&self) -> Vec<String> {
        let Some(config_path) = &self.config_path else {
            return Vec::new();
        };
        let layers = Self::layers(
            Some(&Self::defaults_dir_for(config_path)),
            Some(config_path),
            &Self::conf_d_dir_for(config_path),
        );
        Self::validate_layers(&layers)
    }

    /// `validate_seeded` over layers already read (a reload's).
    pub fn validate_layers(layers: &[(PathBuf, toml::Value)]) -> Vec<String> {
        layers
            .iter()
            .flat_map(|(file, layer)| crate::shape::seeded::read(file, layer).errors)
            .collect()
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
    /// is `[client.providers]` and every `[daemon.providers.<p>.cache]` table. The
    /// config self-watch compares this before and after
    /// (`crate::daemon::config_watch::ChangeKind`).
    pub fn settings_only(mut composed: toml::Value) -> toml::Value {
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

    /// Strict composed parse: every `defaults/*.toml` in `defaults_dir`,
    /// `main_content`, and every `conf.d/*.toml` in `conf_d_dir` must ALL
    /// parse, or the whole call fails. Used by the config self-watch's
    /// restart-gate (`src/daemon/config_watch.rs`), which must never restart
    /// into a composed config where any file is broken — unlike
    /// `load_composed`'s skip-and-continue.
    pub fn parse_composed(
        main_content: &str,
        defaults_dir: &Path,
        conf_d_dir: &Path,
    ) -> Result<Self, String> {
        let main: toml::Value =
            toml::from_str(main_content).map_err(|e| format!("config.toml: {e}"))?;
        if main.as_table().is_none() {
            return Err("config.toml: top-level value must be a table".to_string());
        }
        let mut base = toml::Value::Table(Default::default());
        let merge_dir = |dir: &Path, base: &mut toml::Value| -> Result<(), String> {
            for file in Self::conf_d_files(dir) {
                let content = std::fs::read_to_string(&file)
                    .map_err(|e| format!("{}: {e}", file.display()))?;
                let overlay: toml::Value =
                    toml::from_str(&content).map_err(|e| format!("{}: {e}", file.display()))?;
                deep_merge_toml(base, overlay);
            }
            Ok(())
        };
        merge_dir(defaults_dir, &mut base)?;
        deep_merge_toml(&mut base, main);
        merge_dir(conf_d_dir, &mut base)?;
        Config::deserialize(base).map_err(|e| e.to_string())
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

    /// Return the config file path if it exists.
    pub fn config_path_if_exists() -> Option<std::path::PathBuf> {
        let xdg = xdg::BaseDirectories::with_prefix("beachcomber");
        xdg.find_config_file("config.toml")
    }

    /// Load environment variables from the configured env file (or default path).
    /// Sets them in the process environment so they're available to ${VAR} expansion
    /// in HTTP headers and the commands a Source's data runs.
    /// Returns the number of variables loaded.
    pub fn load_env_file(&self) -> usize {
        let path = match &self.daemon.env_file {
            Some(p) => PathBuf::from(shellexpand(p)),
            None => {
                // Default: ~/.config/beachcomber/env
                let xdg = xdg::BaseDirectories::with_prefix("beachcomber");
                match xdg.find_config_file("env") {
                    Some(p) => p,
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

/// Returns a list of warning messages for deprecated config keys present
/// in the raw TOML. Does not mutate anything — just a pre-parse pass for
/// startup logging.
pub fn detect_deprecated_keys(toml_text: &str) -> Vec<String> {
    let mut warnings = Vec::new();

    let parsed: toml::Value = match toml::from_str(toml_text) {
        Ok(v) => v,
        Err(_) => return warnings,
    };

    // [lifecycle].{cache_lifespan, grace_period_secs, failure_reattempts, failure_backoff_interval}
    if let Some(lifecycle) = parsed.get("lifecycle").and_then(|v| v.as_table()) {
        if lifecycle.contains_key("cache_lifespan") {
            warnings.push(
                "[lifecycle] cache_lifespan is deprecated and ignored; \
                 cache lifetime is now poll_interval × poll_live_count"
                    .to_string(),
            );
        }
        if lifecycle.contains_key("grace_period_secs") {
            warnings.push(
                "[lifecycle] grace_period_secs is deprecated and ignored; \
                 use poll_interval and poll_live_count instead"
                    .to_string(),
            );
        }
        if lifecycle.contains_key("failure_reattempts") {
            warnings.push(
                "[lifecycle] failure_reattempts is deprecated; \
                 move failback_* keys to the [failback] block"
                    .to_string(),
            );
        }
        if lifecycle.contains_key("failure_backoff_interval") {
            warnings.push(
                "[lifecycle] failure_backoff_interval is deprecated; \
                 move failback_* keys to the [failback] block"
                    .to_string(),
            );
        }
    }

    // [providers.*].{poll_idle_interval, poll_live_interval, cache_lifespan}
    if let Some(providers) = parsed.get("providers").and_then(|v| v.as_table()) {
        for (name, provider) in providers {
            if let Some(table) = provider.as_table() {
                if table.contains_key("poll_idle_interval") {
                    warnings.push(format!(
                        "[providers.{name}] poll_idle_interval is deprecated and \
                         ignored; use fsevents_reinstate and the decay ladder"
                    ));
                }
                if table.contains_key("poll_live_interval") {
                    warnings.push(format!(
                        "[providers.{name}] poll_live_interval is deprecated; \
                         renamed to poll_interval"
                    ));
                }
                if table.contains_key("cache_lifespan") {
                    warnings.push(format!(
                        "[providers.{name}] cache_lifespan is deprecated and \
                         ignored; use poll_interval × poll_live_count"
                    ));
                }
            }
        }
    }

    warnings
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
        let mut cfg = Config::default();
        cfg.daemon.socket_path = Some("/from/config/file.sock".into());
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
        let mut base = toml_val("[providers.git]\nenabled = true\npoll_interval = \"30s\"\n");
        let overlay = toml_val("[providers.git]\npoll_interval = \"10s\"\n");
        deep_merge_toml(&mut base, overlay);
        let git = base.get("providers").unwrap().get("git").unwrap();
        assert_eq!(git.get("enabled").unwrap().as_bool(), Some(true));
        assert_eq!(git.get("poll_interval").unwrap().as_str(), Some("10s"));
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

    // ── load_composed / parse_composed ──────────────────────────────────

    #[test]
    fn load_composed_drop_in_overrides_one_key_rest_of_block_preserved() {
        let tmp = tempfile::tempdir().unwrap();
        let conf_d = tmp.path().join("conf.d");
        std::fs::create_dir(&conf_d).unwrap();
        std::fs::write(
            conf_d.join("01-override.toml"),
            "[providers.git]\npoll_interval = \"10s\"\n",
        )
        .unwrap();

        let main =
            "[providers.git]\nenabled = true\npoll_interval = \"30s\"\npoll_live_count = 5\n";
        let cfg = Config::load_composed(main, &tmp.path().join("defaults"), &conf_d);

        let git = cfg.providers.get("git").unwrap();
        assert_eq!(git.get("enabled").unwrap().as_bool(), Some(true));
        assert_eq!(git.get("poll_live_count").unwrap().as_integer(), Some(5));
        assert_eq!(git.get("poll_interval").unwrap().as_str(), Some("10s"));
    }

    #[test]
    fn load_composed_conf_d_with_no_main_file() {
        let tmp = tempfile::tempdir().unwrap();
        let conf_d = tmp.path().join("conf.d");
        std::fs::create_dir(&conf_d).unwrap();
        std::fs::write(
            conf_d.join("01-daemon.toml"),
            "[daemon]\nlog_level = \"debug\"\n",
        )
        .unwrap();

        // No main config content at all (absent file).
        let cfg = Config::load_composed("", &tmp.path().join("defaults"), &conf_d);
        assert_eq!(cfg.daemon.log_level, "debug");
    }

    #[test]
    fn load_composed_invalid_drop_in_is_skipped_rest_still_composed() {
        let tmp = tempfile::tempdir().unwrap();
        let conf_d = tmp.path().join("conf.d");
        std::fs::create_dir(&conf_d).unwrap();
        std::fs::write(conf_d.join("01-bad.toml"), "this is not [ valid toml").unwrap();
        std::fs::write(
            conf_d.join("02-good.toml"),
            "[daemon]\nlog_level = \"debug\"\n",
        )
        .unwrap();

        let cfg = Config::load_composed("[daemon]\n", &tmp.path().join("defaults"), &conf_d);
        assert_eq!(cfg.daemon.log_level, "debug");
    }

    #[test]
    fn parse_composed_fails_when_any_conf_d_file_is_invalid() {
        let tmp = tempfile::tempdir().unwrap();
        let conf_d = tmp.path().join("conf.d");
        std::fs::create_dir(&conf_d).unwrap();
        std::fs::write(conf_d.join("01-bad.toml"), "this is not [ valid toml").unwrap();

        let result = Config::parse_composed("[daemon]\n", &tmp.path().join("defaults"), &conf_d);
        assert!(result.is_err());
    }

    #[test]
    fn parse_composed_succeeds_when_composed_set_is_all_valid() {
        let tmp = tempfile::tempdir().unwrap();
        let conf_d = tmp.path().join("conf.d");
        std::fs::create_dir(&conf_d).unwrap();
        std::fs::write(
            conf_d.join("01-daemon.toml"),
            "[daemon]\nlog_level = \"debug\"\n",
        )
        .unwrap();

        let result = Config::parse_composed("[daemon]\n", &tmp.path().join("defaults"), &conf_d);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().daemon.log_level, "debug");
    }
}
