use crate::config::ScriptProviderConfig;
use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use std::process::Command;
use std::sync::OnceLock;
use tracing::debug;

pub struct ScriptProvider {
    name: String,
    config: ScriptProviderConfig,
}

impl ScriptProvider {
    pub fn new(name: &str, config: ScriptProviderConfig) -> Self {
        Self {
            name: name.to_string(),
            config,
        }
    }
}

impl Provider for ScriptProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: self.name.clone(),
            sources: vec![self.single_source_meta()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(ScriptSingleSource {
            name: self.name.clone(),
            config: self.config.clone(),
            meta: OnceLock::new(),
        })]
    }
}

impl ScriptProvider {
    fn single_source_meta(&self) -> SourceMetadata {
        build_source_meta(&self.name, &self.config)
    }
}

struct ScriptSingleSource {
    name: String,
    config: ScriptProviderConfig,
    meta: OnceLock<SourceMetadata>,
}

impl Source for ScriptSingleSource {
    fn metadata(&self) -> &SourceMetadata {
        self.meta.get_or_init(|| build_source_meta(&self.name, &self.config))
    }

    fn execute(&self, path: Option<&str>) -> SourceResult {
        let output = if cfg!(target_os = "windows") {
            Command::new("cmd")
                .args(["/C", &self.config.command])
                .current_dir(path.unwrap_or("."))
                .output()
        } else {
            Command::new("sh")
                .args(["-c", &self.config.command])
                .current_dir(path.unwrap_or("."))
                .output()
        };

        let output = match output.ok() {
            Some(o) => o,
            None => return SourceResult::new(),
        };
        if !output.status.success() {
            debug!(
                "Script provider '{}' failed with exit code {:?}",
                self.name,
                output.status.code()
            );
            return SourceResult::new();
        }

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if stdout.is_empty() {
            return SourceResult::new();
        }

        let output_format = self.config.output.as_deref().unwrap_or("json");

        let maybe_result = match output_format {
            "kv" => parse_kv_output(&stdout),
            "text" => parse_text_output(&stdout),
            _ => parse_json_output(&stdout),
        };

        maybe_result.unwrap_or_else(SourceResult::new)
    }
}

fn build_source_meta(name: &str, config: &ScriptProviderConfig) -> SourceMetadata {
    let poll_secs = config
        .invalidation
        .as_ref()
        .and_then(|i| i.poll.as_ref())
        .and_then(|s| crate::scheduler::parse_duration_secs_pub(s))
        .unwrap_or(30);

    let watch_patterns = config.invalidation.as_ref().and_then(|i| i.watch.clone());

    let is_global = config.scope.as_deref() != Some("path");
    let scope = if is_global {
        SourceScope::Global
    } else {
        SourceScope::PathScoped
    };

    let (invalidation, keep_alive) = match watch_patterns {
        Some(patterns) => {
            if scope == SourceScope::Global {
                // Global watch: use abs_paths only; patterns meaningless for global.
                // Fall back to poll since we have no abs_paths to watch.
                let _ = patterns;
                (
                    InvalidationStrategy::Poll { interval_secs: poll_secs },
                    KeepAlive::Polls(2),
                )
            } else {
                (
                    InvalidationStrategy::WatchAndPoll {
                        patterns,
                        abs_paths: vec![],
                        interval_secs: poll_secs,
                    },
                    KeepAlive::Polls(2),
                )
            }
        }
        None => (
            InvalidationStrategy::Poll { interval_secs: poll_secs },
            KeepAlive::Polls(2),
        ),
    };

    let fields = config
        .fields
        .as_ref()
        .map(|f| {
            f.iter()
                .map(|(fname, spec)| FieldSchema {
                    name: fname.clone(),
                    field_type: match spec.field_type() {
                        "int" => FieldType::Int,
                        "bool" => FieldType::Bool,
                        "float" => FieldType::Float,
                        _ => FieldType::String,
                    },
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    // Use a sentinel if no fields declared (dynamic output).
    let fields = if fields.is_empty() {
        vec![FieldSchema { name: "<field>".into(), field_type: FieldType::String }]
    } else {
        fields
    };

    SourceMetadata {
        name: "main".into(),
        fields,
        scope,
        invalidation,
        keep_alive,
        failback: FailbackConfig { reattempts: 3, interval_secs: 60 },
        fsevents_reinstate: false,
    }
}

fn parse_json_output(stdout: &str) -> Option<SourceResult> {
    let parsed: serde_json::Value = serde_json::from_str(stdout).ok()?;
    let obj = parsed.as_object()?;

    let mut result = SourceResult::new();
    for (key, val) in obj {
        let value = match val {
            serde_json::Value::String(s) => Value::String(s.clone()),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Value::Int(i)
                } else if let Some(f) = n.as_f64() {
                    Value::Float(f)
                } else {
                    Value::String(n.to_string())
                }
            }
            serde_json::Value::Bool(b) => Value::Bool(*b),
            other => Value::String(other.to_string()),
        };
        result.insert(key.clone(), value);
    }
    Some(result)
}

fn parse_kv_output(stdout: &str) -> Option<SourceResult> {
    let mut result = SourceResult::new();

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            result.insert(
                key.trim().to_string(),
                Value::String(value.trim().to_string()),
            );
        }
    }

    if result.fields.is_empty() {
        return None;
    }
    Some(result)
}

fn parse_text_output(stdout: &str) -> Option<SourceResult> {
    if stdout.is_empty() {
        return None;
    }
    let mut result = SourceResult::new();
    result.insert("value".to_string(), Value::String(stdout.to_string()));
    Some(result)
}
