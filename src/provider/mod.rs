pub mod asdf;
pub mod aws;
pub mod battery;
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
pub mod python;
pub mod registry;
pub mod script;
pub mod sudo;
pub mod talos;
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

/// Maximum nesting depth preserved when converting JSON into [`Value`].
///
/// Beyond this, a subtree is stored as its JSON text rather than dropped, so no
/// data is lost — it just stops being addressable by path. The bound exists
/// because provider output is untrusted input and recursion is unbounded
/// otherwise.
pub const MAX_JSON_DEPTH: usize = 10;

impl Value {
    /// The single conversion from `serde_json::Value` into a provider [`Value`].
    ///
    /// Every ingestion path — `put`, script, http, library — goes through this.
    /// Do not pattern-match `serde_json::Value` to build a `Value` anywhere else;
    /// four copies of that match previously existed and all four failed to
    /// recurse, so nested objects were silently stringified despite
    /// `docs/canon/field_resolution.md` invariant 12 promising depth-independent
    /// addressing.
    ///
    /// Objects become [`Value::Object`]. Arrays become [`Value::Object`] keyed by
    /// decimal index, matching how array segments are already addressed by path
    /// elsewhere. Null becomes an empty string. Depth is capped at
    /// [`MAX_JSON_DEPTH`].
    pub fn from_json(value: &serde_json::Value) -> Value {
        Self::from_json_at(value, 0)
    }

    fn from_json_at(value: &serde_json::Value, depth: usize) -> Value {
        match value {
            serde_json::Value::String(s) => Value::String(s.clone()),
            serde_json::Value::Bool(b) => Value::Bool(*b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Value::Int(i)
                } else if let Some(f) = n.as_f64() {
                    Value::Float(f)
                } else {
                    Value::String(n.to_string())
                }
            }
            serde_json::Value::Null => Value::String(String::new()),
            serde_json::Value::Object(_) | serde_json::Value::Array(_)
                if depth >= MAX_JSON_DEPTH =>
            {
                Value::String(value.to_string())
            }
            serde_json::Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), Self::from_json_at(v, depth + 1)))
                    .collect(),
            ),
            serde_json::Value::Array(items) => Value::Object(
                items
                    .iter()
                    .enumerate()
                    .map(|(i, v)| (i.to_string(), Self::from_json_at(v, depth + 1)))
                    .collect(),
            ),
        }
    }

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

/// The single dotted-path walk over a provider's field map.
///
/// Every caller that resolves `provider.field.sub…` against a `HashMap<String,
/// Value>` goes through this — `ProviderResult::get_path`, the cache's
/// `get_field`, and the server's source-field lookup. Three copies of this walk
/// previously existed with different depth behaviour.
///
/// Tries `path` as a literal field name first, so a provider declaring a field
/// with a dot in its name still resolves. Otherwise splits on the first `.` and
/// walks into nested [`Value::Object`]s for as many segments as the path has.
///
/// Returns `None` if a segment is absent, or if the walk lands on a non-Object
/// before the path is consumed.
pub fn lookup_path<'a>(fields: &'a HashMap<String, Value>, path: &str) -> Option<&'a Value> {
    if let Some(v) = fields.get(path) {
        return Some(v);
    }
    let (head, rest) = path.split_once('.')?;
    let mut current = fields.get(head)?;
    for segment in rest.split('.') {
        match current {
            Value::Object(map) => current = map.get(segment)?,
            _ => return None,
        }
    }
    Some(current)
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
        lookup_path(&self.fields, path)
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

/// Expand `~`, `$HOME`, `$XDG_CONFIG_HOME`, `$XDG_DATA_HOME`, `$XDG_STATE_HOME`,
/// `$XDG_CACHE_HOME`. Falls back to platform XDG defaults (e.g. `$HOME/.config`
/// when `XDG_CONFIG_HOME` is unset). Returns `None` if `$HOME` is unset and no
/// platform fallback applies. Sources call this in `metadata()` so the
/// scheduler receives canonical absolute paths.
pub fn expand_abs_path(s: &str) -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '~' && (out.is_empty() || out.ends_with('/')) {
            out.push_str(&home);
            continue;
        }
        if c == '$' {
            // Read variable name (alphanumeric + underscore)
            let mut name = String::new();
            while let Some(&nc) = chars.peek() {
                if nc.is_alphanumeric() || nc == '_' {
                    name.push(nc);
                    chars.next();
                } else {
                    break;
                }
            }
            let val = match name.as_str() {
                "HOME" => home.clone(),
                "XDG_CONFIG_HOME" => {
                    std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| format!("{home}/.config"))
                }
                "XDG_DATA_HOME" => std::env::var("XDG_DATA_HOME")
                    .unwrap_or_else(|_| format!("{home}/.local/share")),
                "XDG_STATE_HOME" => std::env::var("XDG_STATE_HOME")
                    .unwrap_or_else(|_| format!("{home}/.local/state")),
                "XDG_CACHE_HOME" => {
                    std::env::var("XDG_CACHE_HOME").unwrap_or_else(|_| format!("{home}/.cache"))
                }
                _ => std::env::var(&name).ok()?,
            };
            out.push_str(&val);
            continue;
        }
        out.push(c);
    }
    let p = std::path::PathBuf::from(out);
    if p.is_absolute() { Some(p) } else { None }
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

    /// Build a result from a JSON object's top-level keys, converting each value
    /// through [`Value::from_json`].
    ///
    /// This is the shared body of every "provider emitted a JSON object" path.
    /// Use it rather than looping and converting at each call site.
    pub fn from_json_object(map: &serde_json::Map<String, serde_json::Value>) -> Self {
        let mut result = Self::new();
        for (key, val) in map {
            result.insert(key.clone(), Value::from_json(val));
        }
        result
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
        // Field name uniqueness across all sources within this provider.
        // Dynamic sentinel names (starting with '<') are placeholders for runtime-resolved
        // field names and may appear in multiple sources within the same provider (e.g. mise
        // has a <tool> sentinel in both its global and project sources). Skip uniqueness
        // checking for those names.
        let mut field_owners: HashMap<String, String> = HashMap::new();
        for s in &self.sources {
            for f in &s.fields {
                if f.name.starts_with('<') {
                    // Dynamic sentinel — allowed to appear in multiple sources.
                    continue;
                }
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
        (InvalidationStrategy::Watch { .. }, KeepAlive::Duration(_), SourceScope::PathScoped) => {
            Ok(())
        }
        (InvalidationStrategy::Watch { .. }, KeepAlive::Never, SourceScope::Global) => Ok(()),
        _ => Err(format!(
            "provider '{}' source '{}': KeepAlive variant does not match strategy/scope. \
             Polls(K) requires Poll/WatchAndPoll; Duration(secs) requires Watch + PathScoped; \
             Never requires Watch + Global.",
            provider, s.name
        )),
    }?;
    // Global Watch sources should use abs_paths only
    if let (InvalidationStrategy::Watch { patterns, .. }, SourceScope::Global) =
        (&s.invalidation, &s.scope)
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

    /// Absolute files this PathScoped instance should watch, derived from its
    /// scope `path`. Default: none. An env-selected file source (kube/talos)
    /// returns the concrete files encoded in its (possibly ':'-joined) path so
    /// the scheduler can watch each one even though the path is not a single
    /// watchable directory. Only consulted for Watch/WatchAndPoll PathScoped sources.
    fn watched_files(&self, _path: Option<&str>) -> Vec<std::path::PathBuf> {
        Vec::new()
    }

    /// If true, the request path re-executes this source on every read instead
    /// of serving cache. Only for sources whose execute is a cheap file/syscall
    /// read. Expensive sources (subprocess, worktree scan, network) must return
    /// false and stay event/poll-driven.
    fn read_always(&self) -> bool {
        false
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
        assert!(watch_patterns(&InvalidationStrategy::Poll { interval_secs: 10 }).is_empty());
    }

    fn make_source(name: &str, fields: Vec<&str>) -> SourceMetadata {
        SourceMetadata {
            name: name.into(),
            fields: fields
                .into_iter()
                .map(|n| FieldSchema {
                    name: n.into(),
                    field_type: FieldType::String,
                })
                .collect(),
            scope: SourceScope::Global,
            invalidation: InvalidationStrategy::Poll { interval_secs: 30 },
            keep_alive: KeepAlive::Polls(2),
            failback: FailbackConfig {
                reattempts: 3,
                interval_secs: 30,
            },
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
        let meta = ProviderMetadata {
            name: "x".into(),
            sources: vec![s],
        };
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
        let meta = ProviderMetadata {
            name: "x".into(),
            sources: vec![s],
        };
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
        let meta = ProviderMetadata {
            name: "x".into(),
            sources: vec![s],
        };
        assert!(meta.validate().is_err());
    }

    #[test]
    fn validate_rejects_fsevents_reinstate_on_poll() {
        let mut s = make_source("a", vec!["f1"]);
        s.fsevents_reinstate = true;
        let meta = ProviderMetadata {
            name: "x".into(),
            sources: vec![s],
        };
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

    #[test]
    fn expand_abs_path_resolves_tilde() {
        let home = std::env::var("HOME").expect("HOME set");
        let p = expand_abs_path("~/foo").unwrap();
        assert_eq!(p.to_string_lossy(), format!("{home}/foo"));
    }

    #[test]
    fn expand_abs_path_resolves_xdg_config_home_fallback() {
        let home = std::env::var("HOME").expect("HOME set");
        // Use a value we can predict regardless of caller's env.
        let saved = std::env::var("XDG_CONFIG_HOME").ok();
        // SAFETY: test single-threaded for env mutation
        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
        let p = expand_abs_path("$XDG_CONFIG_HOME/mise").unwrap();
        assert_eq!(p.to_string_lossy(), format!("{home}/.config/mise"));
        if let Some(v) = saved {
            unsafe {
                std::env::set_var("XDG_CONFIG_HOME", v);
            }
        }
    }

    #[test]
    fn expand_abs_path_returns_none_for_relative() {
        assert!(expand_abs_path("relative/path").is_none());
    }
}
