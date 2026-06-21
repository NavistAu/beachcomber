use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use std::collections::HashMap;
use std::path::PathBuf;

pub struct GcloudProvider;

impl Provider for GcloudProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "gcloud_configs".into(),
            sources: vec![config_dir_source_metadata()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(GcloudConfigDir)]
    }
}

fn config_dir_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "config_dir".into(),
        // Dynamic sentinel: actual field names are config names (e.g. "default", "work").
        // Plus a fixed "active_config" field — both declared here so the scheduler knows
        // this source owns them.
        fields: vec![
            FieldSchema {
                name: "active_config".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "<config>".into(),
                field_type: FieldType::Object,
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

struct GcloudConfigDir;

impl Source for GcloudConfigDir {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(config_dir_source_metadata)
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        let Some(config_dir) = gcloud_config_dir() else {
            return SourceResult::new();
        };

        // Read the default active config name from the active_config file only.
        // $CLOUDSDK_ACTIVE_CONFIG_NAME is a per-shell override resolved client-side
        // via the P2 live.* path (later phase). The daemon serves the file-declared default.
        let active_name: Option<String> = {
            let active_path = config_dir.join("active_config");
            std::fs::read_to_string(&active_path)
                .ok()
                .map(|c| c.trim().to_string())
                .filter(|s| !s.is_empty())
        };

        // Enumerate all configs from configurations/config_<name>/properties
        let configs = read_all_configs(&config_dir);

        let mut result = SourceResult::new();

        // Insert active_config String field if present
        if let Some(ref name) = active_name {
            result.insert("active_config", Value::String(name.clone()));
        }

        // Insert one Object field per config name
        for (name, fields) in configs {
            result.insert(name, Value::Object(fields));
        }

        result
    }
}

/// Enumerate every `configurations/config_<name>/properties` under `config_dir`,
/// parse the `[core]` section for `project` and `account`, and return a map of
/// config_name → {project, account} (skipping configs where both are empty).
fn read_all_configs(config_dir: &std::path::Path) -> HashMap<String, HashMap<String, Value>> {
    let configs_dir = config_dir.join("configurations");
    let Ok(entries) = std::fs::read_dir(&configs_dir) else {
        return HashMap::new();
    };

    let mut result = HashMap::new();

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();

        // Only process directories named config_<name>
        let Some(config_name) = name_str.strip_prefix("config_") else {
            continue;
        };
        if config_name.is_empty() {
            continue;
        }
        // Must be a directory
        if !entry.path().is_dir() {
            continue;
        }

        let properties_path = entry.path().join("properties");
        let Ok(content) = std::fs::read_to_string(&properties_path) else {
            continue;
        };

        let (project, account) = parse_core_section(&content);
        if project.is_empty() && account.is_empty() {
            continue;
        }

        let mut fields: HashMap<String, Value> = HashMap::new();
        if !project.is_empty() {
            fields.insert("project".to_string(), Value::String(project));
        }
        if !account.is_empty() {
            fields.insert("account".to_string(), Value::String(account));
        }

        result.insert(config_name.to_string(), fields);
    }

    result
}

/// Parse the `[core]` section of a gcloud properties file.
/// Returns `(project, account)` — empty string if not found.
fn parse_core_section(content: &str) -> (String, String) {
    let mut project = String::new();
    let mut account = String::new();
    let mut in_core = false;

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_core = line[1..line.len() - 1].trim() == "core";
            continue;
        }
        if in_core && let Some((key, val)) = line.split_once('=') {
            let key = key.trim();
            let val = val.trim().to_string();
            match key {
                "project" => project = val,
                "account" => account = val,
                _ => {}
            }
        }
    }

    (project, account)
}

fn gcloud_config_dir() -> Option<PathBuf> {
    // $CLOUDSDK_CONFIG overrides the default location
    if let Ok(dir) = std::env::var("CLOUDSDK_CONFIG") {
        return Some(PathBuf::from(dir));
    }
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".config").join("gcloud"))
}
