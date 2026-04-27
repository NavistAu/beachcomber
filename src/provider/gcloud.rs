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
                name: "project".into(),
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

        // Read the active configuration's properties
        let properties_path = config_dir.join("properties");
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
            if in_core {
                if let Some(val) = line.strip_prefix("project") {
                    let val = val.trim_start_matches([' ', '=']).trim();
                    project = val.to_string();
                } else if let Some(val) = line.strip_prefix("account") {
                    let val = val.trim_start_matches([' ', '=']).trim();
                    account = val.to_string();
                }
            }
        }

        if project.is_empty() && account.is_empty() {
            return SourceResult::new();
        }

        let mut result = SourceResult::new();
        result.insert("project", Value::String(project));
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
