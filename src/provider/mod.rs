pub mod asdf;
pub mod aws;
pub mod battery;
pub mod conda;
pub mod direnv;
pub mod gcloud;
pub mod git;
pub mod hostname;
pub mod http;
pub mod kubecontext;
pub mod library;
pub mod load;
pub mod mise;
pub mod network;
pub mod op;
pub mod python;
pub mod registry;
pub mod script;
pub mod sudo;
pub mod terraform;
pub mod uname;
#[cfg(target_os = "macos")]
pub mod uptime;
#[cfg(target_os = "linux")]
pub mod uptime_linux;
#[cfg(target_os = "linux")]
pub use uptime_linux as uptime;
pub mod user;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Note: `#[serde(untagged)]` means Float values that are whole numbers (e.g., 42.0)
/// may deserialize as Int on a round-trip. This is acceptable for shell state values
/// which are predominantly strings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Value {
    String(String),
    Int(i64),
    Bool(bool),
    Float(f64),
    Object(HashMap<String, Value>),
}

impl Value {
    pub fn as_text(&self) -> String {
        match self {
            Value::String(s) => s.clone(),
            Value::Int(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Object(map) => {
                let mut items: Vec<String> = map
                    .iter()
                    .map(|(k, v)| format!("{k}={}", v.as_text()))
                    .collect();
                items.sort();
                items.join(",")
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderResult {
    pub fields: HashMap<String, Value>,
}

impl ProviderResult {
    pub fn new() -> Self {
        Self {
            fields: HashMap::new(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.fields.get(key)
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(&self.fields).unwrap_or(serde_json::Value::Null)
    }

    pub fn to_kv_text(&self) -> String {
        let mut lines: Vec<String> = self
            .fields
            .iter()
            .map(|(k, v)| format!("{}={}", k, v.as_text()))
            .collect();
        lines.sort();
        let mut out = lines.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        out
    }

    pub fn insert(&mut self, key: impl Into<String>, value: Value) {
        self.fields.insert(key.into(), value);
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FieldScope {
    Global,
    #[serde(alias = "path")]
    PathScoped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldSchema {
    pub name: String,
    pub field_type: FieldType,
    pub scope: FieldScope,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FieldType {
    String,
    Int,
    Bool,
    Float,
    Object,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InvalidationStrategy {
    Watch {
        patterns: Vec<String>,
        fallback_poll_secs: Option<u64>,
    },
    Poll {
        interval_secs: u64,
        floor_secs: u64,
    },
    WatchAndPoll {
        patterns: Vec<String>,
        interval_secs: u64,
        floor_secs: u64,
    },
    Once,
}

/// Returns the expected refresh interval for a provider's strategy, in whole seconds.
/// Used to populate `CacheEntry::expected_interval_secs` on write paths so staleness
/// reporting works correctly for sync-miss and rerun writes.
pub fn expected_interval_secs(strategy: &InvalidationStrategy) -> Option<u64> {
    match strategy {
        InvalidationStrategy::Poll { interval_secs, .. } => Some(*interval_secs),
        InvalidationStrategy::WatchAndPoll { interval_secs, .. } => Some(*interval_secs),
        InvalidationStrategy::Watch {
            fallback_poll_secs, ..
        } => *fallback_poll_secs,
        InvalidationStrategy::Once => None,
    }
}

/// Returns the filesystem patterns that should trigger re-execution for a provider.
/// Trailing `/` is stripped from each pattern so ".venv/" and ".venv" are equivalent.
pub fn watch_patterns(strategy: &InvalidationStrategy) -> Vec<String> {
    let raw: &[String] = match strategy {
        InvalidationStrategy::Watch { patterns, .. } => patterns,
        InvalidationStrategy::WatchAndPoll { patterns, .. } => patterns,
        _ => return Vec::new(),
    };
    raw.iter()
        .map(|p| p.trim_end_matches('/').to_string())
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderMetadata {
    pub name: String,
    pub fields: Vec<FieldSchema>,
    pub invalidation: InvalidationStrategy,
}

impl ProviderMetadata {
    /// Returns the scope for a named field, or None if the field is not declared.
    pub fn field_scope(&self, field: &str) -> Option<FieldScope> {
        self.fields
            .iter()
            .find(|f| f.name == field)
            .map(|f| f.scope)
    }

    /// Returns the provider's effective scope: PathScoped if any field is path-scoped,
    /// else Global. Used by resolve_path for whole-provider queries and unknown-field
    /// fallback.
    pub fn inferred_scope(&self) -> FieldScope {
        if self
            .fields
            .iter()
            .any(|f| f.scope == FieldScope::PathScoped)
        {
            FieldScope::PathScoped
        } else {
            FieldScope::Global
        }
    }

    /// Validates provider metadata at registration time. Called from
    /// `ProviderRegistry::register_with_source()` to fail loudly at daemon startup
    /// rather than silently at first query.
    pub fn validate(&self) -> Result<(), String> {
        if self.fields.is_empty() {
            return Err(format!("provider '{}' declares no fields", self.name));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ProviderSource {
    #[default]
    Builtin,
    Script,
    Virtual,
}

pub trait Provider: Send + Sync {
    fn metadata(&self) -> ProviderMetadata;
    fn execute(&self, path: Option<&str>) -> Vec<(Option<String>, ProviderResult)>;

    /// Whether the provider wants `fsevents_reinstate = true` by default
    /// when the user hasn't configured the flag either per-provider or in
    /// the global lifecycle section. Meaningful only for providers with a
    /// `Watch` / `WatchAndPoll` invalidation strategy.
    ///
    /// Rationale: providers whose underlying data rarely changes (e.g. mise
    /// project config, `.envrc`, tool-versions) benefit from staying warm
    /// across shell idle, because a subsequent file event would otherwise
    /// race against the decay→eviction window. Providers with high event
    /// churn (e.g. git over an active repo) may prefer the default `false`
    /// so they drop cleanly once demand stops.
    ///
    /// Default: `false`. Override to opt in.
    fn fsevents_reinstate_default(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_patterns_strips_trailing_slash() {
        let strategy = InvalidationStrategy::Watch {
            patterns: vec![".venv/".into(), "pyproject.toml".into()],
            fallback_poll_secs: None,
        };
        let patterns = watch_patterns(&strategy);
        assert_eq!(
            patterns,
            vec![".venv".to_string(), "pyproject.toml".to_string()]
        );
    }

    #[test]
    fn watch_patterns_handles_watch_and_poll() {
        let strategy = InvalidationStrategy::WatchAndPoll {
            patterns: vec![".git".into()],
            interval_secs: 60,
            floor_secs: 1,
        };
        assert_eq!(watch_patterns(&strategy), vec![".git".to_string()]);
    }

    #[test]
    fn watch_patterns_empty_for_poll_and_once() {
        assert!(
            watch_patterns(&InvalidationStrategy::Poll {
                interval_secs: 10,
                floor_secs: 1
            })
            .is_empty()
        );
        assert!(watch_patterns(&InvalidationStrategy::Once).is_empty());
    }

    #[test]
    fn field_scope_round_trips_through_serde() {
        let fs = FieldSchema {
            name: "branch".to_string(),
            field_type: FieldType::String,
            scope: FieldScope::PathScoped,
        };
        let json = serde_json::to_string(&fs).unwrap();
        let back: FieldSchema = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "branch");
        assert_eq!(back.scope, FieldScope::PathScoped);
    }

    #[test]
    fn field_scope_serializes_as_lowercase_string() {
        let fs = FieldSchema {
            name: "branch".to_string(),
            field_type: FieldType::String,
            scope: FieldScope::Global,
        };
        let json = serde_json::to_string(&fs).unwrap();
        assert!(json.contains(r#""scope":"global""#), "got: {json}");
    }

    #[test]
    fn inferred_scope_is_pathscoped_when_any_field_is_pathscoped() {
        let meta = ProviderMetadata {
            name: "mixed".to_string(),
            fields: vec![
                FieldSchema {
                    name: "a".into(),
                    field_type: FieldType::String,
                    scope: FieldScope::Global,
                },
                FieldSchema {
                    name: "b".into(),
                    field_type: FieldType::String,
                    scope: FieldScope::PathScoped,
                },
            ],
            invalidation: InvalidationStrategy::Once,
        };
        assert_eq!(meta.inferred_scope(), FieldScope::PathScoped);
    }

    #[test]
    fn inferred_scope_is_global_when_all_fields_are_global() {
        let meta = ProviderMetadata {
            name: "globals".to_string(),
            fields: vec![
                FieldSchema {
                    name: "a".into(),
                    field_type: FieldType::String,
                    scope: FieldScope::Global,
                },
                FieldSchema {
                    name: "b".into(),
                    field_type: FieldType::String,
                    scope: FieldScope::Global,
                },
            ],
            invalidation: InvalidationStrategy::Once,
        };
        assert_eq!(meta.inferred_scope(), FieldScope::Global);
    }

    #[test]
    fn field_scope_looks_up_by_name() {
        let meta = ProviderMetadata {
            name: "x".to_string(),
            fields: vec![FieldSchema {
                name: "a".into(),
                field_type: FieldType::String,
                scope: FieldScope::PathScoped,
            }],
            invalidation: InvalidationStrategy::Once,
        };
        assert_eq!(meta.field_scope("a"), Some(FieldScope::PathScoped));
        assert_eq!(meta.field_scope("missing"), None);
    }

    #[test]
    fn validate_fails_on_empty_fields() {
        let meta = ProviderMetadata {
            name: "empty".to_string(),
            fields: vec![],
            invalidation: InvalidationStrategy::Once,
        };
        assert!(meta.validate().is_err());
    }

    #[test]
    fn execute_returns_vec_of_scoped_results() {
        // Pins the Provider trait signature. If this compiles, the signature
        // is correct.
        fn _accept<P: Provider + ?Sized>(
            p: &P,
            path: Option<&str>,
        ) -> Vec<(Option<String>, ProviderResult)> {
            p.execute(path)
        }
        let _ = _accept::<dyn Provider>;
    }

    #[test]
    fn validate_passes_on_normal_metadata() {
        let meta = ProviderMetadata {
            name: "normal".to_string(),
            fields: vec![FieldSchema {
                name: "a".into(),
                field_type: FieldType::String,
                scope: FieldScope::Global,
            }],
            invalidation: InvalidationStrategy::Once,
        };
        assert!(meta.validate().is_ok());
    }
}
