pub mod asdf;
pub mod aws;
pub mod battery;
pub mod config_source;
pub mod direnv;
pub mod gcloud;
pub mod git;
pub mod hostname;
pub mod load;
pub mod mise;
pub mod network;
pub mod python;
pub mod registry;
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

#[cfg(test)]
mod aws_tests;
#[cfg(test)]
mod battery_seam_tests;
#[cfg(test)]
mod gcloud_tests;
#[cfg(test)]
mod git_seam_tests;
#[cfg(test)]
mod mise_tests;
#[cfg(test)]
mod provider_trait_tests;
#[cfg(test)]
mod provider_types_tests;
#[cfg(test)]
mod providers_builtin_tests;
#[cfg(test)]
mod providers_cloud_tests;
#[cfg(test)]
mod providers_devtools_tests;
#[cfg(test)]
mod providers_system_tests;
#[cfg(test)]
mod registry_tests;

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
    /// Every ingestion path — `put`, config Sources — goes through this.
    /// Do not pattern-match `serde_json::Value` to build a `Value` anywhere else;
    /// four copies of that match previously existed and all four failed to
    /// recurse, so nested objects were silently stringified despite
    /// `docs/canon/field_resolution.md` invariant 12 promising depth-independent
    /// addressing.
    ///
    /// Objects become [`Value::Object`]. Arrays become [`Value::Object`] keyed by
    /// decimal index, matching how array segments are already addressed by path
    /// elsewhere. Null is absent: `None`, and a null object member or array
    /// element is omitted (surviving elements keep their original index keys).
    /// Depth is capped at [`MAX_JSON_DEPTH`].
    pub fn from_json(value: &serde_json::Value) -> Option<Value> {
        Self::from_json_at(value, 0)
    }

    fn from_json_at(value: &serde_json::Value, depth: usize) -> Option<Value> {
        Some(match value {
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
            serde_json::Value::Null => return None,
            serde_json::Value::Object(_) | serde_json::Value::Array(_)
                if depth >= MAX_JSON_DEPTH =>
            {
                Value::String(value.to_string())
            }
            serde_json::Value::Object(map) => Value::Object(
                map.iter()
                    .filter_map(|(k, v)| Some((k.clone(), Self::from_json_at(v, depth + 1)?)))
                    .collect(),
            ),
            serde_json::Value::Array(items) => Value::Object(
                items
                    .iter()
                    .enumerate()
                    .filter_map(|(i, v)| Some((i.to_string(), Self::from_json_at(v, depth + 1)?)))
                    .collect(),
            ),
        })
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

/// Write `value` at a dotted tail, creating intermediate [`Value::Object`]s.
///
/// The counterpart of [`lookup_path`] for writes. Whatever occupies the final
/// segment is replaced. A non-object sitting on an intermediate segment is
/// replaced by a fresh object too — the tail names an address, and a scalar in
/// the way is simply what currently occupies a prefix of it.
///
/// An empty tail is a no-op: the root is a map, not a node.
///
/// Iterative: a tail's segment count is caller-supplied, so recursing once per
/// segment would put the stack at its mercy.
pub fn insert_path(fields: &mut HashMap<String, Value>, tail: &str, value: Value) {
    if tail.is_empty() {
        return;
    }
    let mut cursor = fields;
    let mut segments = tail.split('.').peekable();
    while let Some(segment) = segments.next() {
        if segments.peek().is_none() {
            cursor.insert(segment.to_string(), value);
            return;
        }
        let slot = cursor
            .entry(segment.to_string())
            .or_insert_with(|| Value::Object(HashMap::new()));
        if !matches!(slot, Value::Object(_)) {
            *slot = Value::Object(HashMap::new());
        }
        if let Value::Object(map) = slot {
            cursor = map;
        } else {
            return;
        }
    }
}

/// Remove the node at a dotted tail, pruning intermediate objects the removal
/// leaves empty. Returns the removed value, or `None` if nothing is stored
/// there — including when a non-object sits on an intermediate segment, since
/// then the addressed node does not exist.
///
/// Iterative, for the same reason [`insert_path`] is. Three linear passes: one
/// to size each map on the path, one to take the node, one to drop the
/// ancestors the removal emptied. A map empties only if it held exactly the
/// one child on the path, so the sizes say up front how far the pruning
/// reaches and a single removal there takes the whole empty chain.
pub fn remove_path(fields: &mut HashMap<String, Value>, tail: &str) -> Option<Value> {
    if tail.is_empty() {
        return None;
    }
    let segments: Vec<&str> = tail.split('.').collect();
    let (last, parents) = segments.split_last()?;

    let mut sizes: Vec<usize> = Vec::with_capacity(segments.len());
    {
        let mut cursor = &*fields;
        for segment in parents {
            sizes.push(cursor.len());
            match cursor.get(*segment) {
                Some(Value::Object(map)) => cursor = map,
                _ => return None,
            }
        }
        sizes.push(cursor.len());
        if !cursor.contains_key(*last) {
            return None;
        }
    }

    let mut cursor = &mut *fields;
    for segment in parents {
        let Some(Value::Object(map)) = cursor.get_mut(*segment) else {
            return None;
        };
        cursor = map;
    }
    let removed = cursor.remove(*last)?;

    // `cut` is the shallowest ancestor entry the removal empties out.
    let mut cut = parents.len();
    while cut > 0 && sizes[cut] == 1 {
        cut -= 1;
    }
    if cut < parents.len() {
        let mut cursor = &mut *fields;
        for segment in &parents[..cut] {
            let Some(Value::Object(map)) = cursor.get_mut(*segment) else {
                return Some(removed);
            };
            cursor = map;
        }
        cursor.remove(parents[cut]);
    }
    Some(removed)
}

/// Merge `overlay` into `base`, recursing into objects present on both sides.
/// Any non-object on either side replaces what `base` holds.
///
/// This is how a cache entry's source maps combine on read: the maps hold
/// disjoint records, so merge order never decides a value — only which
/// intermediate objects get stitched together.
pub fn deep_merge(base: &mut HashMap<String, Value>, overlay: HashMap<String, Value>) {
    for (k, v) in overlay {
        match (base.get_mut(&k), v) {
            (Some(Value::Object(existing)), Value::Object(incoming)) => {
                deep_merge(existing, incoming);
            }
            (_, v) => {
                base.insert(k, v);
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvalidationStrategy {
    Poll {
        interval_secs: u64,
        /// Polls in the Active window: it lasts `interval_secs × count`.
        count: u32,
    },
    Watch {
        patterns: Vec<String>,
        abs_paths: Vec<String>,
    },
    WatchAndPoll {
        patterns: Vec<String>,
        abs_paths: Vec<String>,
        interval_secs: u64,
        count: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailbackConfig {
    pub reattempts: u32,
    pub interval_secs: u64,
}

/// Returns the expected refresh interval for a provider's strategy, in whole seconds.
/// Used to populate `CacheEntry::expected_interval_secs` on write paths so staleness
/// reporting works correctly for sync-miss and rerun writes.
pub fn expected_interval_secs(strategy: &InvalidationStrategy) -> Option<u64> {
    match strategy {
        InvalidationStrategy::Poll { interval_secs, .. } => Some(*interval_secs),
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
    pub invalidation: InvalidationStrategy,
    pub failback: FailbackConfig,
    /// Whether watches survive decay. Default `true` for Watch/WatchAndPoll
    /// per canon §"fsevents_reinstate default". Meaningless for Poll.
    pub fsevents_reinstate: bool,
    /// Decay steps before eviction, when the Source sets its own (`ttl(...)`
    /// sets 1). `None` takes the provider's `_.decay` prototype.
    pub decay: Option<u8>,
    /// How long one `execute` may take before the engine reports it slow.
    pub budget_execute: std::time::Duration,
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
            if let Some(v) = Value::from_json(val) {
                result.insert(key.clone(), v);
            }
        }
        result
    }
}

/// A sentinel field name (`<tool>`) stands for runtime-resolved field names.
pub(crate) fn is_sentinel(name: &str) -> bool {
    name.starts_with('<')
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderMetadata {
    pub name: String,
    pub sources: Vec<SourceMetadata>,
}

impl ProviderMetadata {
    /// Validates provider metadata at registration time. A provider declares
    /// at least one Source; a provider named only in configuration has none
    /// and is registered through `ProviderRegistry::register_config_named`.
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
        // field names and may appear in multiple sources within the same provider (a config
        // Source and a built-in can both answer for the provider node). Skip uniqueness
        // checking for those names.
        let mut field_owners: HashMap<String, String> = HashMap::new();
        for s in &self.sources {
            for f in &s.fields {
                if is_sentinel(&f.name) {
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
    // fsevents_reinstate is meaningless for Poll
    if matches!(s.invalidation, InvalidationStrategy::Poll { .. }) && s.fsevents_reinstate {
        return Err(format!(
            "provider '{}' source '{}': fsevents_reinstate=true on Poll strategy is meaningless",
            provider, s.name
        ));
    }
    Ok(())
}

/// A Provider is a namespace declaring one or more Sources.
pub trait Provider: Send + Sync {
    fn metadata(&self) -> ProviderMetadata;
    fn sources(&self) -> Vec<Box<dyn Source>>;
}

/// A Source is the unit of refresh. Owns its own invalidation, fields,
/// lifecycle, and failure backoff. Identified by (provider_name, source_name).
///
/// A Source has one instance per root: the records it makes for a directory
/// live at the instance [`Source::root`] names for it, and the one instance at
/// `~` answers for every directory that has no root.
///
/// A new method with a default body must also be forwarded by
/// `provider::registry::Retuned`, which wraps a built-in whose `return`
/// retunes it.
pub trait Source: Send + Sync {
    fn metadata(&self) -> &SourceMetadata;

    /// Produce the instance's records. `dir` is the instance's working
    /// directory: its root, or `$HOME` for the instance at `~`.
    ///
    /// `Err` is a failure: nothing is written, and the scheduler counts it
    /// toward `failback`. `Ok` is a result, an empty one included: it replaces
    /// what the instance stored before, so a Source with nothing to report
    /// clears its records instead of leaving stale ones behind.
    fn execute(&self, dir: Option<&str>) -> Result<SourceResult, String>;

    /// The instance answering for `dir`: `Some(root)` is the instance at that
    /// directory, `None` the one instance at `~`. The default has no roots, so
    /// every directory is served by the instance at `~`.
    ///
    /// A root that cannot be worked out is an `Err` the reader sees, never the
    /// instance at `~`.
    fn root(&self, dir: &str) -> Result<Option<String>, String> {
        let _ = dir;
        Ok(None)
    }

    /// Names the lookup [`Source::root`] performs. Two Sources of one provider
    /// with the same key answer every directory alike, so one request asks
    /// once (git's five Sources share one walk to the repository). The default
    /// is the Source's own name: nothing is shared.
    fn root_key(&self) -> String {
        self.metadata().name.clone()
    }

    /// Whether [`Source::root`] can name a directory: false for a Source with
    /// one instance, at `~`. Decides where a relative `watch` path can be
    /// resolved at all.
    fn has_root(&self) -> bool {
        false
    }

    /// Absolute files this instance should watch, derived from its `root`.
    /// Default: none. A source whose root is not itself a watchable directory
    /// returns the concrete files the scheduler should watch instead. Only
    /// consulted for Watch/WatchAndPoll sources.
    fn watched_files(&self, _root: Option<&str>) -> Vec<std::path::PathBuf> {
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
            count: 2,
        };
        assert_eq!(watch_patterns(&strategy), vec![".git".to_string()]);
    }

    #[test]
    fn watch_patterns_empty_for_poll() {
        assert!(
            watch_patterns(&InvalidationStrategy::Poll {
                interval_secs: 10,
                count: 2
            })
            .is_empty()
        );
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
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 30,
                count: 2,
            },
            failback: FailbackConfig {
                reattempts: 3,
                interval_secs: 30,
            },
            fsevents_reinstate: false,
            decay: None,
            budget_execute: crate::scheduler::lifecycle::DEFAULT_BUDGET_EXECUTE,
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

    // ── dotted-tail writes: insert_path / remove_path / deep_merge ───────────

    fn obj(pairs: &[(&str, Value)]) -> Value {
        Value::Object(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        )
    }

    #[test]
    fn insert_path_creates_intermediate_objects() {
        let mut map = HashMap::new();
        insert_path(&mut map, "some.value", Value::Int(5));
        assert_eq!(map.get("some"), Some(&obj(&[("value", Value::Int(5))])));
    }

    #[test]
    fn insert_path_replaces_a_scalar_in_the_way() {
        let mut map = HashMap::new();
        map.insert("some".to_string(), Value::Int(1));
        insert_path(&mut map, "some.value", Value::Int(5));
        assert_eq!(map.get("some"), Some(&obj(&[("value", Value::Int(5))])));
    }

    #[test]
    fn insert_path_at_the_top_level_replaces_the_field() {
        let mut map = HashMap::new();
        map.insert("a".to_string(), Value::Int(1));
        insert_path(&mut map, "a", Value::String("x".into()));
        assert_eq!(map.get("a"), Some(&Value::String("x".into())));
    }

    #[test]
    fn remove_path_prunes_now_empty_intermediates() {
        let mut map = HashMap::new();
        insert_path(&mut map, "some.value", Value::Int(5));
        assert_eq!(remove_path(&mut map, "some.value"), Some(Value::Int(5)));
        assert!(map.is_empty(), "empty `some` must be pruned, got {map:?}");
    }

    #[test]
    fn remove_path_keeps_siblings() {
        let mut map = HashMap::new();
        insert_path(&mut map, "some.a", Value::Int(1));
        insert_path(&mut map, "some.b", Value::Int(2));
        assert_eq!(remove_path(&mut map, "some.a"), Some(Value::Int(1)));
        assert_eq!(map.get("some"), Some(&obj(&[("b", Value::Int(2))])));
    }

    #[test]
    fn remove_path_returns_none_on_a_miss() {
        let mut map = HashMap::new();
        insert_path(&mut map, "some.value", Value::Int(5));
        assert_eq!(remove_path(&mut map, "some.nope"), None);
        assert_eq!(remove_path(&mut map, "nope"), None);
        // A scalar in the way is not the node addressed, so nothing is removed.
        assert_eq!(remove_path(&mut map, "some.value.deeper"), None);
        assert_eq!(map.get("some"), Some(&obj(&[("value", Value::Int(5))])));
    }

    #[test]
    fn deep_merge_merges_nested_objects() {
        let mut base = HashMap::new();
        insert_path(&mut base, "some.a", Value::Int(1));
        let mut overlay = HashMap::new();
        insert_path(&mut overlay, "some.b", Value::Int(2));
        deep_merge(&mut base, overlay);
        assert_eq!(
            base.get("some"),
            Some(&obj(&[("a", Value::Int(1)), ("b", Value::Int(2))]))
        );
    }

    #[test]
    fn deep_merge_lets_a_scalar_replace_an_object() {
        let mut base = HashMap::new();
        insert_path(&mut base, "some.a", Value::Int(1));
        let mut overlay = HashMap::new();
        overlay.insert("some".to_string(), Value::Int(9));
        deep_merge(&mut base, overlay);
        assert_eq!(base.get("some"), Some(&Value::Int(9)));
    }

    #[test]
    fn non_file_sources_watch_no_explicit_files() {
        for s in super::git::GitProvider.sources() {
            assert!(s.watched_files(Some("/tmp")).is_empty());
        }
    }
}
