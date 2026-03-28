use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub daemon: DaemonConfig,
    pub lifecycle: LifecycleConfig,
    #[serde(default)]
    pub providers: HashMap<String, ScriptProviderConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            daemon: DaemonConfig::default(),
            lifecycle: LifecycleConfig::default(),
            providers: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    pub socket_path: Option<String>,
    pub log_level: String,
    pub provider_timeout_secs: Option<u64>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: None,
            log_level: "info".to_string(),
            provider_timeout_secs: Some(10),
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
            idle_shutdown_secs: Some(300), // 5 minutes
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
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ScriptInvalidation {
    pub poll: Option<String>,
    pub watch: Option<Vec<String>>,
}

impl Config {
    pub fn is_provider_disabled(&self, name: &str) -> bool {
        self.providers.get(name)
            .and_then(|p| p.enabled)
            .map(|e| !e)
            .unwrap_or(false)
    }

    pub fn script_providers(&self) -> Vec<(String, ScriptProviderConfig)> {
        self.providers.iter()
            .filter(|(_, v)| v.provider_type.as_deref() == Some("script") || (!v.command.is_empty() && v.provider_type.is_none()))
            .map(|(name, config)| (name.clone(), config.clone()))
            .collect()
    }

    pub fn load() -> Self {
        let xdg = xdg::BaseDirectories::with_prefix("shellstate");

        match xdg.find_config_file("config.toml") {
            Some(path) => {
                let content = std::fs::read_to_string(&path).unwrap_or_default();
                toml::from_str(&content).unwrap_or_default()
            }
            None => Self::default(),
        }
    }

    pub fn resolve_socket_path(&self) -> PathBuf {
        if let Some(ref path) = self.daemon.socket_path {
            return PathBuf::from(path);
        }

        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            return PathBuf::from(runtime_dir).join("shellstate").join("sock");
        }

        let uid = unsafe { libc::getuid() };
        let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(tmpdir)
            .join(format!("shellstate-{}", uid))
            .join("sock")
    }

    pub fn resolve_log_path(&self) -> PathBuf {
        let xdg = xdg::BaseDirectories::with_prefix("shellstate");

        xdg.get_state_home()
            .unwrap_or_else(|| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
                PathBuf::from(home).join(".local").join("state").join("shellstate")
            })
            .join("daemon.log")
    }
}
