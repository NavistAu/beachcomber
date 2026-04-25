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
    pub fields: Vec<FieldSchema>,
    pub invalidation: InvalidationStrategy,
}

impl ProviderMetadata {
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

    /// Map a candidate path to the provider's canonical project root.
    ///
    /// Path-scoped providers with a "project marker" concept (e.g. git → `.git`,
    /// mise → `mise.toml`, direnv → `.envrc`) should override this to walk up
    /// from the candidate path and return the directory that actually contains
    /// the marker. The scheduler uses the result as the cache key, lifecycle
    /// key, and fs-watch root, so two demands from different subdirectories of
    /// the same project dedupe to a single entry.
    ///
    /// Returns `None` to signal "this provider does not apply to this path"
    /// (e.g. git asked about a directory not inside any repo). The scheduler
    /// treats `None` as decline-demand: no cache entry, no lifecycle entry, no
    /// watch registration.
    ///
    /// Default: identity (returns the input path unchanged). Global providers
    /// are never called here because `FieldScope::Global` short-circuits path
    /// resolution to `None` earlier.
    fn canonical_path(&self, path: Option<&str>) -> Option<String> {
        path.map(|p| p.to_string())
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

    #[test]
    fn validate_fails_on_empty_fields() {
        let meta = ProviderMetadata {
            name: "empty".to_string(),
            fields: vec![],
            invalidation: InvalidationStrategy::Poll { interval_secs: 30 },
        };
        assert!(meta.validate().is_err());
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
            fields: vec![FieldSchema {
                name: "a".into(),
                field_type: FieldType::String,
            }],
            invalidation: InvalidationStrategy::Poll { interval_secs: 30 },
        };
        assert!(meta.validate().is_ok());
    }
}
