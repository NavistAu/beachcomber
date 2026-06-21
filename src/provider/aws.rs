use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct AwsProvider;

impl Provider for AwsProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "aws_profiles".into(),
            sources: vec![config_file_source_metadata()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(AwsConfigFile)]
    }
}

fn config_file_source_metadata() -> SourceMetadata {
    SourceMetadata {
        name: "config_file".into(),
        // Dynamic sentinel: actual field names are profile names (e.g. "default", "staging").
        // Mirrors the <tool> pattern used by mise for dynamic fields.
        fields: vec![FieldSchema {
            name: "<profile>".into(),
            field_type: FieldType::Object,
        }],
        scope: SourceScope::Global,
        // Watch the config file; fall back to poll as backstop.
        invalidation: InvalidationStrategy::WatchAndPoll {
            patterns: vec![],
            abs_paths: aws_config_abs_paths(),
            interval_secs: 60,
        },
        keep_alive: KeepAlive::Polls(2),
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 30,
        },
        fsevents_reinstate: false,
    }
}

/// Returns absolute paths to watch for `~/.aws/config` invalidation.
/// Expanded at metadata time so the scheduler receives canonical paths.
fn aws_config_abs_paths() -> Vec<String> {
    if let Ok(p) = std::env::var("AWS_CONFIG_FILE")
        && !p.is_empty()
    {
        return vec![p];
    }
    if let Ok(home) = std::env::var("HOME") {
        return vec![format!("{home}/.aws/config")];
    }
    vec![]
}

struct AwsConfigFile;

impl Source for AwsConfigFile {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(config_file_source_metadata)
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        let config_path = resolve_aws_config_path();
        let profiles = parse_all_profiles(&config_path);
        let mut result = SourceResult::new();
        for (name, fields) in profiles {
            result.insert(name, Value::Object(fields));
        }
        result
    }
}

/// Resolve the path to the AWS config file.
/// $AWS_CONFIG_FILE overrides (standard AWS SDK convention; also used in tests).
fn resolve_aws_config_path() -> PathBuf {
    if let Ok(p) = std::env::var("AWS_CONFIG_FILE")
        && !p.is_empty()
    {
        return PathBuf::from(p);
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".aws").join("config");
    }
    PathBuf::from("/dev/null")
}

/// Parse every profile section from an AWS INI config file.
///
/// Format: `[default]` is the default profile (key `"default"`);
/// `[profile X]` sections are named profiles (key `"X"`).
/// Each profile captures `region` if present and non-empty.
/// Profiles with no region are omitted.
///
/// Returns a map of profile_name → field map (currently only `region`).
fn parse_all_profiles(path: &Path) -> HashMap<String, HashMap<String, Value>> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };

    let mut profiles: HashMap<String, HashMap<String, Value>> = HashMap::new();
    let mut current_profile: Option<String> = None;
    let mut current_fields: HashMap<String, Value> = HashMap::new();

    for line in content.lines() {
        let line = line.trim();

        if line.starts_with('[') && line.ends_with(']') {
            // Flush the previous profile if it has fields
            if let Some(name) = current_profile.take()
                && !current_fields.is_empty()
            {
                profiles.insert(name, current_fields);
            }
            current_fields = HashMap::new();

            let section = line[1..line.len() - 1].trim();
            current_profile = if section == "default" {
                Some("default".to_string())
            } else if let Some(name) = section.strip_prefix("profile ") {
                let name = name.trim();
                if !name.is_empty() {
                    Some(name.to_string())
                } else {
                    None
                }
            } else {
                // Unknown section type (e.g. [sso-session]) — skip
                None
            };
            continue;
        }

        // Parse key = value within a known profile section
        if current_profile.is_some()
            && let Some((key, val)) = line.split_once('=')
        {
            let key = key.trim();
            let val = val.trim();
            if key == "region" && !val.is_empty() {
                current_fields.insert("region".to_string(), Value::String(val.to_string()));
            }
        }
    }

    // Flush the last profile
    if let Some(name) = current_profile
        && !current_fields.is_empty()
    {
        profiles.insert(name, current_fields);
    }

    profiles
}
