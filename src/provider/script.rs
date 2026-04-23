use crate::config::ScriptProviderConfig;
use crate::provider::{
    FieldSchema, FieldScope, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};
use std::process::Command;
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
        let invalidation = build_invalidation(&self.config);
        let global = self.config.scope.as_deref() != Some("path");

        let fields = self
            .config
            .fields
            .as_ref()
            .map(|f| {
                f.iter()
                    .map(|(name, type_str)| FieldSchema {
                        name: name.clone(),
                        field_type: match type_str.as_str() {
                            "int" => FieldType::Int,
                            "bool" => FieldType::Bool,
                            "float" => FieldType::Float,
                            _ => FieldType::String,
                        },
                        scope: FieldScope::Global,
                    })
                    .collect()
            })
            .unwrap_or_default();

        ProviderMetadata {
            name: self.name.clone(),
            fields,
            invalidation,
            global,
        }
    }

    fn execute(&self, path: Option<&str>) -> Vec<(Option<String>, ProviderResult)> {
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
            None => return Vec::new(),
        };
        if !output.status.success() {
            debug!(
                "Script provider '{}' failed with exit code {:?}",
                self.name,
                output.status.code()
            );
            return Vec::new();
        }

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if stdout.is_empty() {
            return Vec::new();
        }

        let output_format = self.config.output.as_deref().unwrap_or("json");

        let maybe_result = match output_format {
            "kv" => parse_kv_output(&stdout),
            "text" => parse_text_output(&stdout),
            _ => parse_json_output(&stdout),
        };

        let Some(result) = maybe_result else {
            return Vec::new();
        };
        let is_global = self.config.scope.as_deref() != Some("path");
        let scope_path = if is_global {
            None
        } else {
            path.map(|p| p.to_string())
        };
        vec![(scope_path, result)]
    }
}

fn parse_json_output(stdout: &str) -> Option<ProviderResult> {
    let parsed: serde_json::Value = serde_json::from_str(stdout).ok()?;
    let obj = parsed.as_object()?;

    let mut result = ProviderResult::new();
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

fn parse_kv_output(stdout: &str) -> Option<ProviderResult> {
    let mut result = ProviderResult::new();

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

// `stdout` arrives pre-trimmed from the caller at `execute()`; the
// empty-check here is defensive and in normal flow unreachable.
fn parse_text_output(stdout: &str) -> Option<ProviderResult> {
    if stdout.is_empty() {
        return None;
    }
    let mut result = ProviderResult::new();
    result.insert("value".to_string(), Value::String(stdout.to_string()));
    Some(result)
}

fn build_invalidation(config: &ScriptProviderConfig) -> InvalidationStrategy {
    let poll_secs = config
        .invalidation
        .as_ref()
        .and_then(|i| i.poll.as_ref())
        .and_then(|s| crate::scheduler::parse_duration_secs_pub(s));

    let watch_patterns = config.invalidation.as_ref().and_then(|i| i.watch.clone());

    match (watch_patterns, poll_secs) {
        (Some(patterns), Some(secs)) => InvalidationStrategy::WatchAndPoll {
            patterns,
            interval_secs: secs,
            floor_secs: 1,
        },
        (Some(patterns), None) => InvalidationStrategy::Watch {
            patterns,
            fallback_poll_secs: Some(60),
        },
        (None, Some(secs)) => InvalidationStrategy::Poll {
            interval_secs: secs,
            floor_secs: 1,
        },
        (None, None) => InvalidationStrategy::Poll {
            interval_secs: 30,
            floor_secs: 1,
        },
    }
}
