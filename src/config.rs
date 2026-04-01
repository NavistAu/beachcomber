use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

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
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: None,
            log_level: "info".to_string(),
            provider_timeout_secs: Some(10),
            env_file: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LifecycleConfig {
    pub grace_period_secs: u64,
    pub eviction_timeout_secs: u64,
    pub idle_shutdown_secs: Option<u64>,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            grace_period_secs: 30,
            eviction_timeout_secs: 900,
            idle_shutdown_secs: None, // disabled by default — daemon stays alive until killed
        }
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
        let xdg = xdg::BaseDirectories::with_prefix("beachcomber");

        match xdg.find_config_file("config.toml") {
            Some(path) => {
                let content = std::fs::read_to_string(&path).unwrap_or_default();
                toml::from_str(&content).unwrap_or_default()
            }
            None => Self::default(),
        }
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
