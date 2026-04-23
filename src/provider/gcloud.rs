use crate::provider::{
    FieldSchema, FieldScope, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};

pub struct GcloudProvider;

impl Provider for GcloudProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "gcloud".to_string(),
            fields: vec![
                FieldSchema {
                    name: "project".to_string(),
                    field_type: FieldType::String,
                    scope: FieldScope::Global,
                },
                FieldSchema {
                    name: "account".to_string(),
                    field_type: FieldType::String,
                    scope: FieldScope::Global,
                },
            ],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 60,
                floor_secs: 10,
            },
            global: true,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Option<ProviderResult> {
        let config_dir = gcloud_config_dir()?;

        // Read the active configuration's properties
        let properties_path = config_dir.join("properties");
        let content = std::fs::read_to_string(&properties_path).ok()?;

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
            return None;
        }

        let mut result = ProviderResult::new();
        result.insert("project", Value::String(project));
        result.insert("account", Value::String(account));
        Some(result)
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
