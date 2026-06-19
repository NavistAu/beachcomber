use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};

pub struct GcloudProvider;

impl Provider for GcloudProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "gcloud".into(),
            sources: vec![config_source_metadata()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(GcloudConfig)]
    }
}

fn config_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "config".into(),
        fields: vec![
            FieldSchema {
                name: "config_project".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "account".into(),
                field_type: FieldType::String,
            },
        ],
        scope: SourceScope::Global,
        invalidation: InvalidationStrategy::Poll { interval_secs: 60 },
        keep_alive: KeepAlive::Polls(2),
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 30,
        },
        fsevents_reinstate: false,
    }
}

struct GcloudConfig;

impl Source for GcloudConfig {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(config_source_metadata)
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        let Some(config_dir) = gcloud_config_dir() else {
            return SourceResult::new();
        };

        // Resolve the active configuration name from the active_config file only.
        // $CLOUDSDK_ACTIVE_CONFIG_NAME is a per-shell override resolved client-side
        // via the P2 live.* path (later phase). The daemon serves the default config.
        let active_name = {
            let active_path = config_dir.join("active_config");
            let Ok(content) = std::fs::read_to_string(&active_path) else {
                return SourceResult::new();
            };
            let name = content.trim().to_string();
            if name.is_empty() {
                return SourceResult::new();
            }
            name
        };

        // Read the named configuration's properties file.
        let properties_path = config_dir
            .join("configurations")
            .join(format!("config_{}", active_name))
            .join("properties");
        let Some(content) = std::fs::read_to_string(&properties_path).ok() else {
            return SourceResult::new();
        };

        let mut project = String::new();
        let mut account = String::new();
        let mut in_core = false;

        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_core = line == "[core]";
                continue;
            }
            if in_core && let Some((key, val)) = line.split_once('=') {
                let key = key.trim();
                let val = val.trim().to_string();
                match key {
                    "project" => project = val, // TOML key stays "project"
                    "account" => account = val,
                    _ => {}
                }
            }
        }

        if project.is_empty() && account.is_empty() {
            return SourceResult::new();
        }

        let mut result = SourceResult::new();
        result.insert("config_project", Value::String(project));
        result.insert("account", Value::String(account));
        result
    }
}

fn gcloud_config_dir() -> Option<std::path::PathBuf> {
    // Check CLOUDSDK_CONFIG first, then default
    if let Ok(dir) = std::env::var("CLOUDSDK_CONFIG") {
        return Some(std::path::PathBuf::from(dir));
    }
    let home = std::env::var("HOME").ok()?;
    Some(
        std::path::PathBuf::from(home)
            .join(".config")
            .join("gcloud"),
    )
}
