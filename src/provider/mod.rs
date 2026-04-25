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

    /// Look up a value by a dotted path, walking into nested `Value::Object`s.
    ///
    /// First tries `path` as a literal field name (preserves the rare case of
    /// a provider declaring a field with a dot in its name). If not found,
    /// splits on the first `.` and walks further into nested objects.
    ///
    /// Returns `None` if any step of the walk lands on a non-Object value
    /// before the path is consumed, or if any segment is absent.
    ///
    /// Example: for a ProviderResult whose `project` field is
    /// `Value::Object({"rust": "1.94.0", "cargo-nextest": "0.9.133"})`,
    /// `get_path("project.rust")` returns `Some(Value::String("1.94.0"))`.
    pub fn get_path(&self, path: &str) -> Option<&Value> {
        if let Some(v) = self.fields.get(path) {
            return Some(v);
        }
        let (head, rest) = path.split_once('.')?;
        let mut current = self.fields.get(head)?;
        for segment in rest.split('.') {
            match current {
                Value::Object(map) => {
                    current = map.get(segment)?;
                }
                _ => return None,
            }
        }
        Some(current)
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FieldType {
    String,
    Int,
    Bool,
    Float,
    Object,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceScope {
    Global,
    PathScoped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InvalidationStrategy {
    Poll {
        interval_secs: u64,
    },
    Watch {
        patterns: Vec<String>,
        abs_paths: Vec<String>,
    },
    WatchAndPoll {
        patterns: Vec<String>,
        abs_paths: Vec<String>,
        interval_secs: u64,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum KeepAlive {
    /// Valid for `Poll` and `WatchAndPoll`. Entry stays Active for K polls.
    Polls(u32),
    /// Valid for `Watch` (path-scoped). Entry stays Active for K_secs.
    Duration(u64),
    /// Valid only for `Watch` + `Global`. Entry never decays.
    Never,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct FailbackConfig {
    pub reattempts: u32,
    pub interval_secs: u64,
}

/// Returns the expected refresh interval for a provider's strategy, in whole seconds.
/// Used to populate `CacheEntry::expected_interval_secs` on write paths so staleness
/// reporting works correctly for sync-miss and rerun writes.
pub fn expected_interval_secs(strategy: &InvalidationStrategy) -> Option<u64> {
    match strategy {
        InvalidationStrategy::Poll { interval_secs } => Some(*interval_secs),
        InvalidationStrategy::WatchAndPoll { interval_secs, .. } => Some(*interval_secs),
        InvalidationStrategy::Watch { .. } => None,
    }
}

/// Returns the filesystem patterns that should trigger re-execution for a provider.
/// Trailing `/` is stripped from each pattern so ".venv/" and ".venv" are equivalent.
pub fn watch_patterns(strategy: &InvalidationStrategy) -> Vec<String> {
    let raw: &[String] = match strategy {
        InvalidationStrategy::Watch { patterns, .. } => patterns,
        InvalidationStrategy::WatchAndPoll { patterns, .. } => patterns,
        InvalidationStrategy::Poll { .. } => return Vec::new(),
    };
    raw.iter()
        .map(|p| p.trim_end_matches('/').to_string())
        .collect()
}

/// Returns the absolute filesystem paths to watch for a Source's strategy.
/// Sources are responsible for expanding `~` / `$XDG_*` in metadata() before
/// the value reaches this helper.
pub fn watch_abs_paths(strategy: &InvalidationStrategy) -> Vec<String> {
    match strategy {
        InvalidationStrategy::Watch { abs_paths, .. } => abs_paths.clone(),
        InvalidationStrategy::WatchAndPoll { abs_paths, .. } => abs_paths.clone(),
        InvalidationStrategy::Poll { .. } => Vec::new(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceMetadata {
    pub name: String,
    pub fields: Vec<FieldSchema>,
    pub scope: SourceScope,
    pub invalidation: InvalidationStrategy,
    pub keep_alive: KeepAlive,
    pub failback: FailbackConfig,
    /// Whether watches survive decay. Default `true` for Watch/WatchAndPoll
    /// per canon §"fsevents_reinstate default". Meaningless for Poll.
    pub fsevents_reinstate: bool,
}

/// What a Source produces on a successful execute. Disjoint with sibling
/// Sources at the same (provider, path) by registration-time validation.
#[derive(Debug, Clone, Default)]
pub struct SourceResult {
    pub fields: HashMap<String, Value>,
}

impl SourceResult {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn insert(&mut self, key: impl Into<String>, value: Value) {
        self.fields.insert(key.into(), value);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderMetadata {
    pub name: String,
    pub sources: Vec<SourceMetadata>,
}

impl ProviderMetadata {
    /// Validates provider metadata at registration time.
    pub fn validate(&self) -> Result<(), String> {
        if self.sources.is_empty() {
            return Err(format!("provider '{}' declares no sources", self.name));
        }
        // Source name uniqueness
        let mut names = std::collections::HashSet::new();
        for s in &self.sources {
            if !names.insert(&s.name) {
                return Err(format!(
                    "provider '{}' has duplicate source name '{}'",
                    self.name, s.name
                ));
            }
        }
        // Field name uniqueness across all sources within this provider
        let mut field_owners: HashMap<String, String> = HashMap::new();
        for s in &self.sources {
            for f in &s.fields {
                if let Some(prev) = field_owners.insert(f.name.clone(), s.name.clone()) {
                    return Err(format!(
                        "provider '{}' field '{}' declared by both source '{}' and source '{}'",
                        self.name, f.name, prev, s.name
                    ));
                }
            }
        }
        // Per-source validation
        for s in &self.sources {
            validate_source(&self.name, s)?;
        }
        Ok(())
    }
}

fn validate_source(provider: &str, s: &SourceMetadata) -> Result<(), String> {
    if s.fields.is_empty() {
        return Err(format!(
            "provider '{}' source '{}' declares no fields",
            provider, s.name
        ));
    }
    // KeepAlive variant matches strategy
    match (&s.invalidation, &s.keep_alive, &s.scope) {
        (InvalidationStrategy::Poll { .. }, KeepAlive::Polls(_), _) => Ok(()),
        (InvalidationStrategy::WatchAndPoll { .. }, KeepAlive::Polls(_), _) => Ok(()),
        (InvalidationStrategy::Watch { .. }, KeepAlive::Duration(_), SourceScope::PathScoped) => Ok(()),
        (InvalidationStrategy::Watch { .. }, KeepAlive::Never, SourceScope::Global) => Ok(()),
        _ => Err(format!(
            "provider '{}' source '{}': KeepAlive variant does not match strategy/scope. \
             Polls(K) requires Poll/WatchAndPoll; Duration(secs) requires Watch + PathScoped; \
             Never requires Watch + Global.",
            provider, s.name
        )),
    }?;
    // Global Watch sources should use abs_paths only
    if let (InvalidationStrategy::Watch { patterns, .. }, SourceScope::Global) = (&s.invalidation, &s.scope)
        && !patterns.is_empty()
    {
        return Err(format!(
            "provider '{}' source '{}': Global Watch source declares patterns; use abs_paths instead",
            provider, s.name
        ));
    }
    // fsevents_reinstate is meaningless for Poll
    if matches!(s.invalidation, InvalidationStrategy::Poll { .. }) && s.fsevents_reinstate {
        return Err(format!(
            "provider '{}' source '{}': fsevents_reinstate=true on Poll strategy is meaningless",
            provider, s.name
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ProviderSource {
    #[default]
    Builtin,
    Script,
    Virtual,
}

/// A Provider is a namespace declaring one or more Sources.
pub trait Provider: Send + Sync {
    fn metadata(&self) -> ProviderMetadata;
    fn sources(&self) -> Vec<Box<dyn Source>>;
}

/// A Source is the unit of refresh. Owns its own invalidation, scope, fields,
/// lifecycle, and failure backoff. Identified by (provider_name, source_name).
pub trait Source: Send + Sync {
    fn metadata(&self) -> &SourceMetadata;
    fn execute(&self, path: Option<&str>) -> SourceResult;

    /// Map a candidate path to this Source's canonical scope path.
    /// `PathScoped` sources walking to a project marker should override.
    /// Default: identity. Returns `None` to decline demand.
    fn canonical_path(&self, path: Option<&str>) -> Option<String> {
        path.map(|s| s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_scope_round_trips_through_serde() {
        let s = SourceScope::PathScoped;
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("pathscoped"), "got: {json}");
        let back: SourceScope = serde_json::from_str(&json).unwrap();
        assert_eq!(back, SourceScope::PathScoped);
    }

    #[test]
    fn watch_patterns_strips_trailing_slash() {
        let strategy = InvalidationStrategy::Watch {
            patterns: vec![".venv/".into(), "pyproject.toml".into()],
            abs_paths: vec![],
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
            abs_paths: vec![],
            interval_secs: 60,
        };
        assert_eq!(watch_patterns(&strategy), vec![".git".to_string()]);
    }

    #[test]
    fn watch_patterns_empty_for_poll() {
        assert!(
            watch_patterns(&InvalidationStrategy::Poll {
                interval_secs: 10,
            })
            .is_empty()
        );
    }

    fn make_source(name: &str, fields: Vec<&str>) -> SourceMetadata {
        SourceMetadata {
            name: name.into(),
            fields: fields.into_iter().map(|n| FieldSchema {
                name: n.into(),
                field_type: FieldType::String,
            }).collect(),
            scope: SourceScope::Global,
            invalidation: InvalidationStrategy::Poll { interval_secs: 30 },
            keep_alive: KeepAlive::Polls(2),
            failback: FailbackConfig { reattempts: 3, interval_secs: 30 },
            fsevents_reinstate: false,
        }
    }

    #[test]
    fn validate_rejects_duplicate_source_names() {
        let meta = ProviderMetadata {
            name: "x".into(),
            sources: vec![make_source("a", vec!["f1"]), make_source("a", vec!["f2"])],
        };
        assert!(meta.validate().is_err());
    }

    #[test]
    fn validate_rejects_duplicate_field_names_across_sources() {
        let meta = ProviderMetadata {
            name: "x".into(),
            sources: vec![make_source("a", vec!["f1"]), make_source("b", vec!["f1"])],
        };
        let err = meta.validate().unwrap_err();
        assert!(err.contains("declared by both source"));
    }

    #[test]
    fn validate_rejects_polls_keep_alive_on_watch_strategy() {
        let mut s = make_source("a", vec!["f1"]);
        s.invalidation = InvalidationStrategy::Watch {
            patterns: vec![],
            abs_paths: vec!["/foo".into()],
        };
        s.keep_alive = KeepAlive::Polls(2);
        let meta = ProviderMetadata { name: "x".into(), sources: vec![s] };
        assert!(meta.validate().is_err());
    }

    #[test]
    fn validate_rejects_never_keep_alive_on_path_scoped() {
        let mut s = make_source("a", vec!["f1"]);
        s.invalidation = InvalidationStrategy::Watch {
            patterns: vec!["foo".into()],
            abs_paths: vec![],
        };
        s.scope = SourceScope::PathScoped;
        s.keep_alive = KeepAlive::Never;
        let meta = ProviderMetadata { name: "x".into(), sources: vec![s] };
        assert!(meta.validate().is_err());
    }

    #[test]
    fn validate_rejects_global_watch_with_patterns() {
        let mut s = make_source("a", vec!["f1"]);
        s.invalidation = InvalidationStrategy::Watch {
            patterns: vec!["foo".into()],
            abs_paths: vec![],
        };
        s.scope = SourceScope::Global;
        s.keep_alive = KeepAlive::Never;
        let meta = ProviderMetadata { name: "x".into(), sources: vec![s] };
        assert!(meta.validate().is_err());
    }

    #[test]
    fn validate_rejects_fsevents_reinstate_on_poll() {
        let mut s = make_source("a", vec!["f1"]);
        s.fsevents_reinstate = true;
        let meta = ProviderMetadata { name: "x".into(), sources: vec![s] };
        assert!(meta.validate().is_err());
    }

    #[test]
    fn validate_accepts_well_formed_provider() {
        let meta = ProviderMetadata {
            name: "x".into(),
            sources: vec![make_source("a", vec!["f1"]), make_source("b", vec!["f2"])],
        };
        assert!(meta.validate().is_ok());
    }

    #[test]
    fn get_path_returns_scalar_from_nested_object() {
        let mut tools = HashMap::new();
        tools.insert("rust".to_string(), Value::String("1.94.0".to_string()));
        tools.insert(
            "cargo-nextest".to_string(),
            Value::String("0.9.133".to_string()),
        );
        let mut result = ProviderResult::new();
        result.insert("project", Value::Object(tools));

        match result.get_path("project.rust") {
            Some(Value::String(s)) => assert_eq!(s, "1.94.0"),
            other => panic!("expected String('1.94.0'), got {other:?}"),
        }
    }

    #[test]
    fn get_path_literal_dot_field_wins_over_walk() {
        // If a provider declares a field with a literal dot in the name,
        // that takes precedence over the hierarchical walk.
        let mut result = ProviderResult::new();
        result.insert("a.b", Value::String("literal".into()));

        let mut inner = HashMap::new();
        inner.insert("b".to_string(), Value::String("walked".into()));
        result.insert("a", Value::Object(inner));

        match result.get_path("a.b") {
            Some(Value::String(s)) => assert_eq!(s, "literal"),
            other => panic!("expected literal match, got {other:?}"),
        }
    }

    #[test]
    fn get_path_returns_none_for_missing_subkey() {
        let mut inner = HashMap::new();
        inner.insert("rust".to_string(), Value::String("1.94.0".into()));
        let mut result = ProviderResult::new();
        result.insert("project", Value::Object(inner));

        assert!(result.get_path("project.nonesuch").is_none());
    }

    #[test]
    fn get_path_returns_none_when_walking_through_scalar() {
        let mut result = ProviderResult::new();
        result.insert("name", Value::String("host".into()));
        // Can't walk `name` (scalar) into `.inner`.
        assert!(result.get_path("name.inner").is_none());
    }

    #[test]
    fn get_path_works_at_depth_three() {
        let mut inner = HashMap::new();
        inner.insert("leaf".to_string(), Value::String("v".into()));
        let mut middle = HashMap::new();
        middle.insert("mid".to_string(), Value::Object(inner));
        let mut result = ProviderResult::new();
        result.insert("top", Value::Object(middle));

        match result.get_path("top.mid.leaf") {
            Some(Value::String(s)) => assert_eq!(s, "v"),
            other => panic!("expected depth-3 walk to succeed, got {other:?}"),
        }
    }

    #[test]
    fn validate_passes_on_normal_metadata() {
        let meta = ProviderMetadata {
            name: "normal".to_string(),
            sources: vec![make_source("a", vec!["f1"])],
        };
        assert!(meta.validate().is_ok());
    }
}
