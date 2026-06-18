use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use std::path::{Path, PathBuf};

pub struct AwsProvider;

impl Provider for AwsProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "aws".into(),
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
        fields: vec![FieldSchema {
            name: "config_region".into(),
            field_type: FieldType::String,
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
        let Some(region) = parse_default_profile_region(&config_path) else {
            return SourceResult::new();
        };
        let mut result = SourceResult::new();
        result.insert("config_region", Value::String(region));
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

/// Parse the `region` key from the `[default]` profile in an AWS INI config file.
/// AWS config INI format: `[default]` or `[profile name]` sections; keys are `key = value`.
/// Returns `None` if the file is absent, unreadable, or has no `[default]` region.
fn parse_default_profile_region(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut in_default = false;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            // `[default]` is the literal default section name in ~/.aws/config.
            in_default = line == "[default]";
            continue;
        }
        if in_default
            && let Some((key, val)) = line.split_once('=')
            && key.trim() == "region"
        {
            let v = val.trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}
