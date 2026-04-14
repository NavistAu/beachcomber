use serde::{Deserialize, Deserializer};
use std::collections::HashMap;
use std::path::PathBuf;

/// Deserialize a value that can be either a string ("30s") or an integer (30, treated as seconds).
fn deserialize_duration_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de;

    struct StringOrInt;

    impl<'de> de::Visitor<'de> for StringOrInt {
        type Value = String;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a duration string (\"30s\") or integer (seconds)")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<String, E> {
            Ok(v.to_string())
        }

        fn visit_string<E: de::Error>(self, v: String) -> Result<String, E> {
            Ok(v)
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<String, E> {
            Ok(format!("{v}s"))
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<String, E> {
            Ok(format!("{v}s"))
        }
    }

    deserializer.deserialize_any(StringOrInt)
}
use std::time::Duration;

/// Parse a duration string like "500ms", "30s", "5m", "1h" into a Duration.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(stripped) = s.strip_suffix("ms") {
        return stripped
            .trim()
            .parse::<u64>()
            .ok()
            .map(Duration::from_millis);
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

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub daemon: DaemonConfig,
    pub lifecycle: LifecycleConfig,
    #[serde(default)]
    pub providers: HashMap<String, ScriptProviderConfig>,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LifecycleConfig {
    #[serde(
        alias = "grace_period_secs",
        deserialize_with = "deserialize_duration_string"
    )]
    pub cache_lifespan: String,
    pub eviction_timeout_secs: u64,
    pub idle_shutdown_secs: Option<u64>,
    pub failure_reattempts: u32,
    pub failure_backoff_interval: String,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            cache_lifespan: "30s".to_string(),
            eviction_timeout_secs: 900,
            idle_shutdown_secs: None,
            failure_reattempts: 3,
            failure_backoff_interval: "1s".to_string(),
        }
    }
}

impl LifecycleConfig {
    pub fn cache_lifespan_duration(&self) -> Duration {
        parse_duration(&self.cache_lifespan).unwrap_or(Duration::from_secs(30))
    }

    pub fn failure_backoff_duration(&self) -> Duration {
        parse_duration(&self.failure_backoff_interval).unwrap_or(Duration::from_secs(1))
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ScriptProviderConfig {
    #[serde(rename = "type")]
    pub provider_type: Option<String>,
    pub command: String,
    pub invalidation: Option<ScriptInvalidation>,
    pub fields: Option<HashMap<String, String>>,
    pub output: Option<String>,
    pub scope: Option<String>,
    pub enabled: Option<bool>,
    pub poll_secs: Option<u64>,
    pub poll_floor_secs: Option<u64>,
    // HTTP provider fields (used when type = "http")
    pub url: Option<String>,
    pub method: Option<String>,
    pub headers: Option<HashMap<String, String>>,
    pub body: Option<String>,
    pub extract: Option<String>,
    // Library provider fields (used when type = "library")
    pub library_path: Option<String>,
    // New configurable backoff fields (override lifecycle defaults)
    pub poll_live_interval: Option<String>,
    pub poll_idle_interval: Option<String>,
    pub cache_lifespan: Option<String>,
    pub failure_reattempts: Option<u32>,
    pub failure_backoff_interval: Option<String>,
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
    pub fn is_provider_disabled(&self, name: &str) -> bool {
        self.providers
            .get(name)
            .and_then(|p| p.enabled)
            .map(|e| !e)
            .unwrap_or(false)
    }

    /// Resolve the cache lifespan for a provider. Per-provider overrides lifecycle default.
    pub fn resolve_cache_lifespan(&self, provider_name: &str) -> Duration {
        self.providers
            .get(provider_name)
            .and_then(|p| p.cache_lifespan.as_ref())
            .and_then(|s| parse_duration(s))
            .unwrap_or_else(|| {
                parse_duration(&self.lifecycle.cache_lifespan).unwrap_or(Duration::from_secs(30))
            })
    }

    pub fn resolve_failure_reattempts(&self, provider_name: &str) -> u32 {
        self.providers
            .get(provider_name)
            .and_then(|p| p.failure_reattempts)
            .unwrap_or(self.lifecycle.failure_reattempts)
    }

    pub fn resolve_failure_backoff_interval(&self, provider_name: &str) -> Duration {
        self.providers
            .get(provider_name)
            .and_then(|p| p.failure_backoff_interval.as_ref())
            .and_then(|s| parse_duration(s))
            .unwrap_or_else(|| {
                parse_duration(&self.lifecycle.failure_backoff_interval)
                    .unwrap_or(Duration::from_secs(1))
            })
    }

    pub fn resolve_poll_idle_interval(&self, provider_name: &str) -> Option<Duration> {
        self.providers
            .get(provider_name)
            .and_then(|p| p.poll_idle_interval.as_ref())
            .and_then(|s| parse_duration(s))
    }

    pub fn resolve_poll_live_interval(&self, provider_name: &str) -> Option<u64> {
        // poll_live_interval takes precedence over poll_secs
        let provider_config = self.providers.get(provider_name);
        if let Some(config) = provider_config {
            if let Some(ref interval) = config.poll_live_interval {
                return parse_duration(interval).map(|d| d.as_secs());
            }
            if let Some(secs) = config.poll_secs {
                return Some(secs);
            }
        }
        None
    }

    pub fn script_providers(&self) -> Vec<(String, ScriptProviderConfig)> {
        self.providers
            .iter()
            .filter(|(_, v)| {
                v.provider_type.as_deref() == Some("script")
                    || (!v.command.is_empty() && v.provider_type.is_none())
            })
            .map(|(name, config)| (name.clone(), config.clone()))
            .collect()
    }

    pub fn library_providers(&self) -> Vec<(String, ScriptProviderConfig)> {
        self.providers
            .iter()
            .filter(|(_, v)| v.provider_type.as_deref() == Some("library"))
            .map(|(name, config)| (name.clone(), config.clone()))
            .collect()
    }

    pub fn http_providers(&self) -> Vec<(String, HttpProviderConfig)> {
        self.providers
            .iter()
            .filter(|(_, v)| v.provider_type.as_deref() == Some("http"))
            .map(|(name, config)| {
                (
                    name.clone(),
                    HttpProviderConfig {
                        provider_type: config.provider_type.clone(),
                        url: config.url.clone().unwrap_or_default(),
                        method: config.method.clone(),
                        headers: config.headers.clone(),
                        body: config.body.clone(),
                        extract: config.extract.clone(),
                        invalidation: config.invalidation.clone(),
                    },
                )
            })
            .collect()
    }

    pub fn load() -> Self {
        match Self::config_path_if_exists() {
            Some(path) => {
                let content = std::fs::read_to_string(&path).unwrap_or_default();
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
        if let Some(ref path) = self.daemon.socket_path {
            return PathBuf::from(path);
        }

        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            return PathBuf::from(runtime_dir).join("beachcomber").join("sock");
        }

        let uid = unsafe { libc::getuid() };
        let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(tmpdir)
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
