use crate::provider::FieldScope;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tracing::warn;

/// Parse a duration string like "30s", "5m", "1h", or whole-second "ms" values (e.g. "2000ms")
/// into a Duration. Returns None for sub-second `ms` values and non-whole-second multiples.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(stripped) = s.strip_suffix("ms") {
        let n = stripped.trim().parse::<u64>().ok()?;
        if n < 1000 || n % 1000 != 0 {
            return None;
        }
        return Some(Duration::from_secs(n / 1000));
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
        .map(|n| Duration::from_secs(n * multiplier))
}

// ── Source-knob keys that are ONLY valid inside [providers.<name>.<source>] blocks,
// not at the top-level [providers.<name>] block.
const SOURCE_KNOB_KEYS: &[&str] = &[
    "poll_interval",
    "poll_count",
    "fsevent_patterns",
    "fsevent_abs_paths",
    "fsevent_lifespan",
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

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub daemon: DaemonConfig,
    pub lifecycle: LifecycleConfig,
    pub failback: FailbackGlobalConfig,
    /// Raw TOML value for providers. Use accessor methods to get typed views.
    /// Stored as raw toml::Value so we can distinguish scalar provider-level keys
    /// from nested per-source sub-tables.
    #[serde(default)]
    pub providers: HashMap<String, toml::Value>,
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
    /// making them available to ${VAR} expansion in HTTP headers, script commands, etc.
    /// Default: ~/.config/beachcomber/env
    pub env_file: Option<String>,
    /// How often the watchdog checks the scheduler heartbeat. Default: disabled (None).
    /// Example: "30s", "1m".
    pub watchdog_interval: Option<String>,
    /// How long the heartbeat can be stale before the watchdog restarts the scheduler.
    /// Default: 3x watchdog_interval.
    /// Example: "90s", "3m".
    pub watchdog_threshold: Option<String>,
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
        }
    }
}

impl DaemonConfig {
    pub fn watchdog_interval_duration(&self) -> Option<Duration> {
        self.watchdog_interval
            .as_ref()
            .and_then(|s| parse_duration(s))
    }

    pub fn watchdog_threshold_duration(&self) -> Option<Duration> {
        if let Some(ref s) = self.watchdog_threshold {
            parse_duration(s)
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
    /// Global default poll keep-alive count.
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
        parse_duration(&self.interval).unwrap_or(Duration::from_secs(1))
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
    pub fsevent_lifespan: Option<String>,
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
                "fsevent_lifespan" => {
                    let s = val.as_str().ok_or_else(|| {
                        format!("{block_name}: key 'fsevent_lifespan' must be a duration string")
                    })?;
                    out.fsevent_lifespan = Some(s.to_string());
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
                         fsevent_abs_paths, fsevent_lifespan, fsevent_reinstates, \
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
            || self.fsevent_lifespan.is_some()
            || self.fsevent_reinstates.is_some())
            && kind == StrategyKind::Poll
        {
            return Err(format!(
                "{block_name}: keys fsevent_patterns/fsevent_abs_paths/fsevent_lifespan/\
                     fsevent_reinstates are not valid for type = \"poll\". \
                     Use type = \"fsevent\" or \"fsevent_poll\"."
            ));
        }

        // fsevent_lifespan is forbidden for global fsevent (pure-watch global never decays)
        if let (Some(_), Some(scope)) = (&self.fsevent_lifespan, &self.scope)
            && scope == "global"
            && kind == StrategyKind::Fsevent
        {
            return Err(format!(
                "{block_name}: fsevent_lifespan is forbidden for \
                     scope = \"global\" + type = \"fsevent\" (pure-watch global never decays)."
            ));
        }

        Ok(())
    }
}

/// Per-source config for external backends (script / http / library).
///
/// Used when a provider is declared with `backend = "script"` (or `"http"`, `"library"`)
/// and each source lives in its own `[providers.<name>.<source>]` sub-table.
///
/// Accepts both declaration keys (`command`, `url`, `fields`, …) and all knob keys
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

    // ── Script-backend declaration keys ──────────────────────────────────────
    pub command: Option<String>,
    pub output: Option<String>,

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
    pub fsevent_lifespan: Option<String>,
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

/// One `[providers.<name>]` block with `backend = "script"`, paired with its parsed sources.
pub type ScriptProviderBlocks = Vec<(String, Vec<ExternalSourceConfig>)>;

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
                // Script
                "command" => {
                    out.command = Some(
                        val.as_str()
                            .ok_or_else(|| format!("{block_name}: 'command' must be a string"))?
                            .to_string(),
                    );
                }
                "output" => {
                    out.output = Some(
                        val.as_str()
                            .ok_or_else(|| format!("{block_name}: 'output' must be a string"))?
                            .to_string(),
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
                "fsevent_lifespan" => {
                    out.fsevent_lifespan = Some(
                        val.as_str()
                            .ok_or_else(|| {
                                format!(
                                    "{block_name}: 'fsevent_lifespan' must be a duration string"
                                )
                            })?
                            .to_string(),
                    );
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
                         Valid external-source keys are: type, scope, enabled, command, output, \
                         url, method, headers, body, extract, default_timeout, fields, \
                         poll_interval, poll_count, fsevent_patterns, fsevent_abs_paths, \
                         fsevent_lifespan, fsevent_reinstates, failback_count, failback_interval"
                    ));
                }
            }
        }

        Ok(out)
    }
}

/// A script/library field declaration. Either a bare type string (legacy:
/// inherits provider-level scope) or a table with explicit type + optional
/// per-field scope override.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum FieldSpec {
    Simple(String),
    Detailed {
        #[serde(rename = "type")]
        field_type: String,
        scope: Option<String>,
    },
}

impl FieldSpec {
    pub fn field_type(&self) -> &str {
        match self {
            FieldSpec::Simple(t) => t,
            FieldSpec::Detailed { field_type, .. } => field_type,
        }
    }

    pub fn explicit_scope(&self) -> Option<FieldScope> {
        match self {
            FieldSpec::Simple(_) => None,
            FieldSpec::Detailed { scope, .. } => scope.as_deref().and_then(|s| match s {
                "global" => Some(FieldScope::Global),
                "path" | "pathscoped" => Some(FieldScope::PathScoped),
                _ => None,
            }),
        }
    }
}

/// Resolve the effective FieldScope for a named field under a script/library
/// provider: use the field's explicit scope if declared, else the
/// provider-level default (`scope = "path"` → PathScoped, otherwise Global).
pub fn resolve_field_scope(config: &ScriptProviderConfig, field: &str) -> FieldScope {
    let explicit = config
        .fields
        .as_ref()
        .and_then(|fields| fields.get(field))
        .and_then(|spec| spec.explicit_scope());
    if let Some(s) = explicit {
        return s;
    }
    match config.scope.as_deref() {
        Some("path") | Some("pathscoped") => FieldScope::PathScoped,
        _ => FieldScope::Global,
    }
}

/// Configuration for a script/library/http provider extracted from the raw providers map.
/// This preserves backward compatibility for Phase 4 which will restructure external backends.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ScriptProviderConfig {
    #[serde(rename = "type")]
    pub provider_type: Option<String>,
    pub command: String,
    pub invalidation: Option<ScriptInvalidation>,
    pub fields: Option<HashMap<String, FieldSpec>>,
    pub output: Option<String>,
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
    pub invalidation: Option<ScriptInvalidation>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ScriptInvalidation {
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
            && let Some(d) = parse_duration(s)
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
            && let Some(d) = parse_duration(s)
        {
            return d;
        }
        // 2. Source-declared default
        if let Some(d) = source_default {
            return d;
        }
        // 3. [lifecycle] global then compile-time default
        parse_duration(&self.lifecycle.poll_interval).unwrap_or(Duration::from_secs(60))
    }

    /// Resolve poll keep-alive count for a (provider, source) pair.
    pub fn resolve_poll_live_count(&self, provider_name: &str) -> u32 {
        self.resolve_poll_live_count_for_source(provider_name, None, None)
    }

    /// Resolve poll keep-alive count for a (provider, source) pair with optional source override.
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
    // 1. Legacy single-source (backward compat): provider-level block carries all
    //    config. Used when there is NO `backend` key (old `type = "script"` shape).
    //    `script_providers()`, `library_providers()`, `http_providers()`.
    //
    // 2. Phase 4 multi-source: provider-level block has `backend = "..."` key;
    //    each source lives in a per-source sub-table. The provider-level block may
    //    also carry `library_path` (library) or `default_timeout` (http).
    //    `multi_script_providers()`, `multi_library_providers()`, `multi_http_providers()`.

    fn as_script_provider_config(val: &toml::Value) -> Option<ScriptProviderConfig> {
        // For external backends (script/library/http), deserialize the full provider-level
        // table as ScriptProviderConfig. Sub-table entries like `fields` and `invalidation`
        // are part of the external backend config shape (Phase 4 will restructure these).
        let table = val.as_table()?.clone();
        table.try_into().ok()
    }

    /// Return the `backend` value for a provider's top-level block, if present.
    fn backend_for(val: &toml::Value) -> Option<&str> {
        val.get("backend").and_then(|v| v.as_str())
    }

    pub fn script_providers(&self) -> Vec<(String, ScriptProviderConfig)> {
        self.providers
            .iter()
            .filter_map(|(name, val)| {
                // Skip Phase 4 multi-source style (has `backend` key).
                if Self::backend_for(val).is_some() {
                    return None;
                }
                let cfg = Self::as_script_provider_config(val)?;
                let is_script = cfg.provider_type.as_deref() == Some("script")
                    || (!cfg.command.is_empty() && cfg.provider_type.is_none());
                if is_script {
                    Some((name.clone(), cfg))
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn library_providers(&self) -> Vec<(String, ScriptProviderConfig)> {
        self.providers
            .iter()
            .filter_map(|(name, val)| {
                // Skip Phase 4 multi-source style (has `backend` key).
                if Self::backend_for(val).is_some() {
                    return None;
                }
                let cfg = Self::as_script_provider_config(val)?;
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
                let cfg = Self::as_script_provider_config(val)?;
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
            if matches!(sub_key.as_str(), "invalidation" | "fields" | "virtual") {
                // "invalidation"/"fields" appear in legacy flat shape; in multi-source they
                // live inside per-source blocks instead. "virtual" holds client-side
                // virtual-field expressions (canon invariant 7: evaluated CLI-side, never
                // by the daemon). Skip all three at provider level.
                continue;
            }
            let block_name = format!("[providers.{prov_name}.{sub_key}]");
            let mut src = ExternalSourceConfig::from_toml_table(&block_name, sub_table)?;
            src.name = sub_key.clone();
            sources.push(src);
        }
        Ok(sources)
    }

    /// Multi-source script providers: `backend = "script"` + per-source sub-tables.
    /// Each source sub-table must contain `command`.
    pub fn multi_script_providers(&self) -> Result<ScriptProviderBlocks, String> {
        let mut result = Vec::new();
        for (name, val) in &self.providers {
            if Self::backend_for(val) != Some("script") {
                continue;
            }
            let sources = Self::parse_external_sources(name, val)?;
            // Validate: each source must have a command.
            for src in &sources {
                if src.command.is_none() {
                    return Err(format!(
                        "provider '{}' source '{}' (backend = script): \
                         missing required key 'command'",
                        name, src.name
                    ));
                }
            }
            result.push((name.clone(), sources));
        }
        Ok(result)
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

            // Check for old flat source-knob keys at the provider level.
            // These are rejected for ALL providers (built-in and external).
            // Exception: script/library/http providers still use some of these
            // keys (failure_reattempts, failure_backoff_interval, poll_interval,
            // poll_live_count, fsevents_reinstate, poll_secs) at provider level
            // until Phase 4. We detect if this looks like an external backend.
            let is_external_backend = table
                .get("type")
                .and_then(|v| v.as_str())
                .map(|t| matches!(t, "script" | "library" | "http"))
                .unwrap_or(false)
                || table
                    .get("backend")
                    .and_then(|v| v.as_str())
                    .map(|b| matches!(b, "script" | "library" | "http"))
                    .unwrap_or(false)
                || table.get("command").is_some()
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

                // Skip the `virtual` sub-table — it holds client-side virtual-field
                // expressions (canon invariant 7: evaluated CLI-side, never by the daemon).
                if sub_key == "virtual" {
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

    /// Return per-provider path expression overrides from config.
    ///
    /// Reads the `path` scalar key from each `[providers.<name>]` section.
    /// Returns a map of provider_name → path_expression_string.
    pub fn path_expressions(&self) -> std::collections::HashMap<String, String> {
        let mut out = std::collections::HashMap::new();
        for (provider_name, provider_val) in &self.providers {
            let toml::Value::Table(table) = provider_val else {
                continue;
            };
            if let Some(toml::Value::String(expr)) = table.get("path") {
                out.insert(provider_name.clone(), expr.clone());
            }
        }
        out
    }

    /// Return per-provider virtual field expression overrides from config.
    ///
    /// Reads the `virtual` sub-table from each `[providers.<name>]` section.
    /// TOML key `"virtual"` is read as a string literal — legal in Rust even though
    /// `virtual` is a reserved keyword as a bare identifier.
    ///
    /// Returns a map of (provider_name, field_name) → expression_string.
    /// Source knobs (siblings of `virtual`) are not included — no exclusion list needed.
    pub fn virtual_fields(&self) -> std::collections::HashMap<(String, String), String> {
        let mut out = std::collections::HashMap::new();
        for (provider_name, provider_val) in &self.providers {
            let toml::Value::Table(table) = provider_val else {
                continue;
            };
            // Read the `virtual` sub-table. "virtual" as a string literal is legal in Rust.
            let Some(toml::Value::Table(virtual_table)) = table.get("virtual") else {
                continue;
            };
            for (field, val) in virtual_table {
                if let toml::Value::String(expr) = val {
                    out.insert((provider_name.clone(), field.clone()), expr.clone());
                }
            }
        }
        out
    }

    pub fn load() -> Self {
        match Self::config_path_if_exists() {
            Some(path) => {
                let content = std::fs::read_to_string(&path).unwrap_or_default();
                for warning in detect_deprecated_keys(&content) {
                    warn!("{}", warning);
                }
                toml::from_str(&content).unwrap_or_default()
            }
            None => Self::default(),
        }
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
    /// in HTTP headers, script commands, etc.
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
        // 1. Config-file override (highest priority)
        if let Some(ref path) = self.daemon.socket_path {
            return PathBuf::from(path);
        }

        // 2. BEACHCOMBER_SOCKET env var
        if let Some(env_path) = std::env::var_os("BEACHCOMBER_SOCKET") {
            let p = std::path::PathBuf::from(env_path);
            if !p.as_os_str().is_empty() {
                return p;
            }
        }

        // 3. Stable per-user default. Consults no session-scoped environment
        // (TMPDIR, XDG_RUNTIME_DIR): singleton enforcement is per-socket-path,
        // so session-scoped inputs yield one daemon per session, not per user.
        // See docs/canon/singleton.md §"Canonical socket path resolution".
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
}
