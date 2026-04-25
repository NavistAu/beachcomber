use crate::config::{ExternalFieldDecl, ExternalSourceConfig, ScriptProviderConfig};
use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use std::process::Command;
use std::sync::OnceLock;
use tracing::debug;

// ── ScriptProvider ─────────────────────────────────────────────────────────────
//
// Two construction paths:
//
// 1. `ScriptProvider::new(name, ScriptProviderConfig)` — single-source, backward
//    compatible. Used by old `type = "script"` TOML and by existing tests.
//
// 2. `ScriptProvider::with_sources(name, Vec<ExternalSourceConfig>)` — multi-source.
//    Used by Phase 4 `backend = "script"` TOML.
//
// Both paths produce the same Provider/Source structure; the difference is in
// how the SourceMetadata is built and how many sources exist.

/// Stores everything ScriptProvider::sources() needs to reconstruct per-source
/// objects without downcasting the trait objects.
struct SourceEntry {
    meta: SourceMetadata,
    command: String,
    output_format: Option<String>,
}

pub struct ScriptProvider {
    name: String,
    entries: Vec<SourceEntry>,
}

impl ScriptProvider {
    /// Create a single-source provider from a ScriptProviderConfig (legacy path).
    pub fn new(name: &str, config: ScriptProviderConfig) -> Self {
        let meta = build_source_meta_from_legacy(name, &config);
        let entry = SourceEntry {
            command: config.command.clone(),
            output_format: config.output.clone(),
            meta,
        };
        Self { name: name.to_string(), entries: vec![entry] }
    }

    /// Create a multi-source provider from Phase 4 per-source ExternalSourceConfig list.
    pub fn with_sources(name: &str, source_configs: Vec<ExternalSourceConfig>) -> Self {
        let entries = source_configs
            .into_iter()
            .map(|cfg| {
                let meta = build_source_meta_from_external(&cfg);
                SourceEntry {
                    command: cfg.command.clone().unwrap_or_default(),
                    output_format: cfg.output.clone(),
                    meta,
                }
            })
            .collect();
        Self { name: name.to_string(), entries }
    }
}

impl Provider for ScriptProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: self.name.clone(),
            sources: self.entries.iter().map(|e| e.meta.clone()).collect(),
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        self.entries
            .iter()
            .map(|e| {
                Box::new(ScriptCommandSource {
                    source_name: e.meta.name.clone(),
                    command: e.command.clone(),
                    output_format: e.output_format.clone(),
                    meta: OnceLock::new(),
                    meta_value: e.meta.clone(),
                }) as Box<dyn Source>
            })
            .collect()
    }
}

// ── ScriptCommandSource ────────────────────────────────────────────────────────

struct ScriptCommandSource {
    source_name: String,
    command: String,
    output_format: Option<String>,
    meta: OnceLock<SourceMetadata>,
    meta_value: SourceMetadata,
}

impl Source for ScriptCommandSource {
    fn metadata(&self) -> &SourceMetadata {
        self.meta.get_or_init(|| self.meta_value.clone())
    }

    fn execute(&self, path: Option<&str>) -> SourceResult {
        let output = if cfg!(target_os = "windows") {
            Command::new("cmd")
                .args(["/C", &self.command])
                .current_dir(path.unwrap_or("."))
                .output()
        } else {
            Command::new("sh")
                .args(["-c", &self.command])
                .current_dir(path.unwrap_or("."))
                .output()
        };

        let output = match output.ok() {
            Some(o) => o,
            None => return SourceResult::new(),
        };
        if !output.status.success() {
            debug!(
                "Script source '{}' failed with exit code {:?}",
                self.source_name,
                output.status.code()
            );
            return SourceResult::new();
        }

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if stdout.is_empty() {
            return SourceResult::new();
        }

        let fmt = self.output_format.as_deref().unwrap_or("json");
        let maybe_result = match fmt {
            "kv" => parse_kv_output(&stdout),
            "text" => parse_text_output(&stdout),
            _ => parse_json_output(&stdout),
        };

        maybe_result.unwrap_or_else(SourceResult::new)
    }
}

// ── Metadata builders ─────────────────────────────────────────────────────────

fn build_source_meta_from_legacy(_name: &str, config: &ScriptProviderConfig) -> SourceMetadata {
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

pub(crate) fn build_source_meta_from_external(cfg: &ExternalSourceConfig) -> SourceMetadata {
    let strategy_type = cfg.strategy_type.as_deref().unwrap_or("poll");

    let scope = match cfg.scope.as_deref() {
        Some("path") => SourceScope::PathScoped,
        _ => SourceScope::Global,
    };

    let poll_secs = cfg
        .poll_interval
        .as_ref()
        .and_then(|s| crate::scheduler::parse_duration_secs_pub(s))
        .unwrap_or(30);

    let poll_count = cfg.poll_count.unwrap_or(2);

    let patterns = cfg.fsevent_patterns.clone().unwrap_or_default();
    let abs_paths = cfg.fsevent_abs_paths.clone().unwrap_or_default();

    let (invalidation, keep_alive) = match strategy_type {
        "fsevent" => {
            if scope == SourceScope::Global {
                (
                    InvalidationStrategy::Watch {
                        patterns: vec![],
                        abs_paths: abs_paths.clone(),
                    },
                    KeepAlive::Never,
                )
            } else {
                let lifespan_secs = cfg
                    .fsevent_lifespan
                    .as_ref()
                    .and_then(|s| crate::scheduler::parse_duration_secs_pub(s))
                    .unwrap_or(300);
                (
                    InvalidationStrategy::Watch {
                        patterns: patterns.clone(),
                        abs_paths: abs_paths.clone(),
                    },
                    KeepAlive::Duration(lifespan_secs),
                )
            }
        }
        "fsevent_poll" => (
            InvalidationStrategy::WatchAndPoll {
                patterns: patterns.clone(),
                abs_paths: abs_paths.clone(),
                interval_secs: poll_secs,
            },
            KeepAlive::Polls(poll_count),
        ),
        _ => (
            InvalidationStrategy::Poll { interval_secs: poll_secs },
            KeepAlive::Polls(poll_count),
        ),
    };

    let fields = build_fields_from_decls(cfg.fields.as_deref());

    let failback_reattempts = cfg.failback_count.unwrap_or(3);
    let failback_secs = cfg
        .failback_interval
        .as_ref()
        .and_then(|s| crate::scheduler::parse_duration_secs_pub(s))
        .unwrap_or(60);

    SourceMetadata {
        name: cfg.name.clone(),
        fields,
        scope,
        invalidation,
        keep_alive,
        failback: FailbackConfig {
            reattempts: failback_reattempts,
            interval_secs: failback_secs,
        },
        fsevents_reinstate: cfg.fsevent_reinstates.unwrap_or(true),
    }
}

fn build_fields_from_decls(decls: Option<&[ExternalFieldDecl]>) -> Vec<FieldSchema> {
    match decls {
        Some(d) if !d.is_empty() => d
            .iter()
            .map(|f| FieldSchema {
                name: f.name.clone(),
                field_type: match f.field_type.as_str() {
                    "int" => FieldType::Int,
                    "bool" => FieldType::Bool,
                    "float" => FieldType::Float,
                    _ => FieldType::String,
                },
            })
            .collect(),
        _ => vec![FieldSchema { name: "<field>".into(), field_type: FieldType::String }],
    }
}

// ── Output parsers ────────────────────────────────────────────────────────────

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
