//! Source-aware request planning: turns a raw request key plus an optional path
//! hint into a resolved `QueryPlan` (provider, effective path, parsed target,
//! metadata suffix, and the set of Sources the query demands).
//!
//! This is the single place that understands key syntax. `Get` and `Watch` both
//! build a `QueryPlan` here so they share identical resolution semantics.

use crate::provider::SourceScope;
use crate::provider::registry::ProviderRegistry;

/// The set of metadata suffix names the server recognises. Keep this in sync
/// with the `match meta` post-processing arms in `server::handle_request`.
pub const KNOWN_METADATA_SUFFIXES: &[&str] = &["age", "stale", "fresh", "cache", "source"];

/// Parse a metadata suffix from a key. "git.branch:fresh" → ("git.branch", Some("fresh")).
/// Only suffixes listed in KNOWN_METADATA_SUFFIXES are stripped; anything else passes
/// through as part of the key. Metadata suffix splitting must run BEFORE key parsing
/// so the field/suffix disambiguation is correct.
pub fn split_metadata_suffix(key: &str) -> (&str, Option<&str>) {
    if let Some((base, meta)) = key.rsplit_once(':')
        && KNOWN_METADATA_SUFFIXES.contains(&meta)
    {
        return (base, Some(meta));
    }
    (key, None)
}

/// Parse a request key into one of four forms, disambiguating source vs field
/// for 2-segment keys using the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyParse {
    /// "provider"
    Provider(String),
    /// "provider.field" — x is a field name (not a registered source)
    Field(String, String),
    /// "provider.source" — x is a registered source name
    Source(String, String),
    /// "provider.source.field"
    SourceField(String, String, String),
}

/// Disambiguates between Field and Source forms for 2-segment keys.
/// Source name takes precedence over field name when both could match.
/// For 3-part keys `p.s.f`: if `s` is a registered source name for `p`, this
/// is a SourceField lookup; otherwise `p.s` is a field whose value is an Object
/// and `f` is a key within that object (nested field path).
pub fn parse_key(key: &str, registry: &ProviderRegistry) -> KeyParse {
    let parts: Vec<&str> = key.split('.').collect();
    match parts.as_slice() {
        [p] => KeyParse::Provider(p.to_string()),
        [p, x] => {
            if registry.source(p, x).is_some() {
                KeyParse::Source(p.to_string(), x.to_string())
            } else {
                KeyParse::Field(p.to_string(), x.to_string())
            }
        }
        [p, s, f] => {
            if registry.source(p, s).is_some() {
                KeyParse::SourceField(p.to_string(), s.to_string(), f.to_string())
            } else {
                // s is a field name whose value is an Object; f is a sub-key.
                // Encode the nested path as "s.f" in the Field variant.
                KeyParse::Field(p.to_string(), format!("{s}.{f}"))
            }
        }
        _ => KeyParse::Provider(key.to_string()),
    }
}

/// Resolve the effective path for a request. If any source at this provider is
/// PathScoped, the requested path is used (canonicalized). If all sources are
/// Global (or the provider is unknown), returns None.
///
/// For path-scoped providers, also attempts canonical_path() via the first
/// path-scoped source to walk to the project root.
pub fn resolve_path(
    key: &str,
    requested_path: Option<&str>,
    registry: &ProviderRegistry,
) -> Option<String> {
    let parts: Vec<&str> = key.split('.').collect();
    let provider = parts[0];

    // Virtual providers don't declare sources, but they can be stored with a path.
    // Pass the raw path through (canonicalized) so path-keyed virtual data is accessible.
    if registry.is_virtual(provider) {
        return requested_path.map(|p| {
            let path = std::path::Path::new(p);
            if path.is_relative() {
                std::env::current_dir()
                    .ok()
                    .and_then(|cwd| cwd.join(path).canonicalize().ok())
                    .map(|abs| abs.to_string_lossy().to_string())
                    .unwrap_or_else(|| p.to_string())
            } else {
                path.canonicalize()
                    .map(|abs| abs.to_string_lossy().to_string())
                    .unwrap_or_else(|_| p.to_string())
            }
        });
    }

    let any_path_scoped = registry
        .provider_sources(provider)
        .map(|ss| ss.iter().any(|s| s.scope == SourceScope::PathScoped))
        .unwrap_or(false);

    if !any_path_scoped {
        return None;
    }

    let raw = requested_path.map(|p| {
        let path = std::path::Path::new(p);
        if path.is_relative() {
            std::env::current_dir()
                .ok()
                .and_then(|cwd| cwd.join(path).canonicalize().ok())
                .map(|abs| abs.to_string_lossy().to_string())
                .unwrap_or_else(|| p.to_string())
        } else {
            path.canonicalize()
                .map(|abs| abs.to_string_lossy().to_string())
                .unwrap_or_else(|_| p.to_string())
        }
    })?;

    // Provider-level canonicalization: find the first path-scoped source and
    // ask it to canonicalize (walk to project root, etc.).
    if let Some(sources) = registry.provider_sources(provider) {
        for sm in sources {
            if sm.scope == SourceScope::PathScoped
                && let Some(src) = registry.source(provider, &sm.name)
                && let Some(canonical) = src.canonical_path(Some(&raw))
            {
                return Some(canonical);
            }
        }
    }

    Some(raw)
}

/// Which Sources a query demands warming for.
///
/// Approach A from the design spec: an explicit enum keeps field→source
/// resolution in this layer while leaving scope/path policy in the scheduler.
/// `Sources(vec![])` means "warm nothing" (e.g. an unknown field) and is
/// deliberately distinct from `All`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceDemand {
    /// Whole-provider query: warm every applicable Source (scheduler applies scope filter).
    All,
    /// Field/source query: warm only these named Sources.
    Sources(Vec<String>),
}

/// A resolved, source-aware request. Built once per request and consumed by
/// both `Get` and `Watch` so they share identical resolution semantics.
#[derive(Debug, Clone)]
pub struct QueryPlan {
    pub provider: String,
    pub effective_path: Option<String>,
    pub target: KeyParse,
    pub meta: Option<String>,
    pub demand: SourceDemand,
}

impl QueryPlan {
    /// Resolve a raw request key + optional path hint into a plan.
    /// `raw_key` may carry a metadata suffix (e.g. `git.branch:fresh`).
    pub fn build(raw_key: &str, path_hint: Option<&str>, registry: &ProviderRegistry) -> QueryPlan {
        let (stripped_key, meta) = split_metadata_suffix(raw_key);
        let effective_path = resolve_path(stripped_key, path_hint, registry);
        let target = parse_key(stripped_key, registry);

        let provider = match &target {
            KeyParse::Provider(p) => p.clone(),
            KeyParse::Field(p, _) => p.clone(),
            KeyParse::Source(p, _) => p.clone(),
            KeyParse::SourceField(p, _, _) => p.clone(),
        };

        let demand = match &target {
            KeyParse::Provider(_) => SourceDemand::All,
            KeyParse::Source(_, s) => SourceDemand::Sources(vec![s.clone()]),
            KeyParse::SourceField(_, s, _) => SourceDemand::Sources(vec![s.clone()]),
            KeyParse::Field(p, f) => {
                // A field's demand is its owning Source only (canon §150/§268).
                // The owning source is matched on the top-level field name (nested
                // sub-paths like "project.rust" are owned by whoever owns "project").
                let head = f.split('.').next().unwrap_or(f);
                match registry.source_for_field(p, head) {
                    Some(src) => SourceDemand::Sources(vec![src.to_string()]),
                    None => SourceDemand::Sources(Vec::new()),
                }
            }
        };

        QueryPlan {
            provider,
            effective_path,
            target,
            meta: meta.map(|s| s.to_string()),
            demand,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::registry::ProviderRegistry;

    #[test]
    fn parse_key_recognises_provider() {
        let reg = ProviderRegistry::with_defaults();
        assert!(matches!(parse_key("git", &reg), KeyParse::Provider(p) if p == "git"));
    }

    #[test]
    fn parse_key_recognises_source_when_registered() {
        let reg = ProviderRegistry::with_defaults();
        // git has a "refs" source.
        assert!(
            matches!(parse_key("git.refs", &reg), KeyParse::Source(p, s) if p == "git" && s == "refs")
        );
    }

    #[test]
    fn parse_key_falls_back_to_field_when_no_such_source() {
        let reg = ProviderRegistry::with_defaults();
        assert!(
            matches!(parse_key("git.branch", &reg), KeyParse::Field(p, f) if p == "git" && f == "branch")
        );
    }

    #[test]
    fn parse_key_recognises_source_field() {
        let reg = ProviderRegistry::with_defaults();
        assert!(
            matches!(parse_key("git.refs.branch", &reg), KeyParse::SourceField(p, s, f) if p == "git" && s == "refs" && f == "branch")
        );
    }

    #[test]
    fn parse_key_unknown_provider_yields_field() {
        let reg = ProviderRegistry::with_defaults();
        assert!(
            matches!(parse_key("nope.field", &reg), KeyParse::Field(p, f) if p == "nope" && f == "field")
        );
    }

    #[test]
    fn split_metadata_suffix_strips_known() {
        assert_eq!(
            split_metadata_suffix("git.branch:fresh"),
            ("git.branch", Some("fresh"))
        );
    }

    #[test]
    fn split_metadata_suffix_passes_through_unknown() {
        assert_eq!(
            split_metadata_suffix("git.branch:bogus"),
            ("git.branch:bogus", None)
        );
    }

    #[test]
    fn build_provider_demands_all() {
        let reg = ProviderRegistry::with_defaults();
        let plan = QueryPlan::build("git", None, &reg);
        assert!(matches!(plan.demand, SourceDemand::All));
        assert_eq!(plan.provider, "git");
    }

    #[test]
    fn build_field_demands_only_owning_source() {
        let reg = ProviderRegistry::with_defaults();
        // branch is owned by the refs source.
        let plan = QueryPlan::build("git.branch", None, &reg);
        match plan.demand {
            SourceDemand::Sources(v) => assert_eq!(v, vec!["refs".to_string()]),
            other => panic!("expected Sources([refs]), got {other:?}"),
        }
    }

    #[test]
    fn build_source_demands_that_source() {
        let reg = ProviderRegistry::with_defaults();
        let plan = QueryPlan::build("git.refs", None, &reg);
        // Disambiguation: git.refs must parse as a Source target, not a Field,
        // since refs is a registered source. This pins the path, not just the outcome.
        assert!(matches!(plan.target, KeyParse::Source(_, ref s) if s == "refs"));
        match plan.demand {
            SourceDemand::Sources(v) => assert_eq!(v, vec!["refs".to_string()]),
            other => panic!("expected Sources([refs]), got {other:?}"),
        }
    }

    #[test]
    fn build_source_field_demands_that_source() {
        let reg = ProviderRegistry::with_defaults();
        let plan = QueryPlan::build("git.refs.branch", None, &reg);
        match plan.demand {
            SourceDemand::Sources(v) => assert_eq!(v, vec!["refs".to_string()]),
            other => panic!("expected Sources([refs]), got {other:?}"),
        }
    }

    #[test]
    fn build_unknown_field_demands_nothing() {
        let reg = ProviderRegistry::with_defaults();
        let plan = QueryPlan::build("git.totally_bogus_field", None, &reg);
        match plan.demand {
            SourceDemand::Sources(v) => assert!(v.is_empty(), "unknown field warms nothing"),
            other => panic!("expected Sources([]), got {other:?}"),
        }
    }

    #[test]
    fn build_strips_metadata_suffix() {
        let reg = ProviderRegistry::with_defaults();
        let plan = QueryPlan::build("git.branch:fresh", None, &reg);
        assert_eq!(plan.meta.as_deref(), Some("fresh"));
        match plan.demand {
            SourceDemand::Sources(v) => assert_eq!(v, vec!["refs".to_string()]),
            other => panic!("expected Sources([refs]), got {other:?}"),
        }
    }
}

#[cfg(test)]
mod resolve_path_tests {
    use super::*;
    use crate::provider::registry::ProviderRegistry;
    use crate::provider::{
        FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
        ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope,
    };

    struct FakeSource(SourceMetadata);
    impl Source for FakeSource {
        fn metadata(&self) -> &SourceMetadata {
            &self.0
        }
        fn execute(&self, _path: Option<&str>) -> SourceResult {
            SourceResult::new()
        }
    }

    struct FakeProvider {
        name: String,
        scope: SourceScope,
    }

    impl Provider for FakeProvider {
        fn metadata(&self) -> ProviderMetadata {
            let (invalidation, keep_alive) = match self.scope {
                SourceScope::Global => (
                    InvalidationStrategy::Watch {
                        patterns: vec![],
                        abs_paths: vec![],
                    },
                    KeepAlive::Never,
                ),
                SourceScope::PathScoped => (
                    InvalidationStrategy::Poll { interval_secs: 30 },
                    KeepAlive::Polls(2),
                ),
            };
            ProviderMetadata {
                name: self.name.clone(),
                sources: vec![SourceMetadata {
                    name: "main".into(),
                    fields: vec![FieldSchema {
                        name: "v".into(),
                        field_type: FieldType::String,
                    }],
                    scope: self.scope,
                    invalidation,
                    keep_alive,
                    failback: FailbackConfig {
                        reattempts: 3,
                        interval_secs: 30,
                    },
                    fsevents_reinstate: false,
                }],
            }
        }

        fn sources(&self) -> Vec<Box<dyn Source>> {
            vec![Box::new(FakeSource(self.metadata().sources[0].clone()))]
        }
    }

    fn registry_with(providers: Vec<Box<dyn Provider>>) -> ProviderRegistry {
        let mut reg = ProviderRegistry::new();
        for p in providers {
            reg.register(p).unwrap();
        }
        reg
    }

    #[test]
    fn global_provider_ignores_explicit_path() {
        let reg = registry_with(vec![Box::new(FakeProvider {
            name: "hostname".into(),
            scope: SourceScope::Global,
        })]);
        let result = resolve_path("hostname", Some("/tmp"), &reg);
        assert_eq!(result, None, "global provider must ignore explicit path");
    }

    #[test]
    fn path_scoped_provider_honors_explicit_path() {
        let reg = registry_with(vec![Box::new(FakeProvider {
            name: "git".into(),
            scope: SourceScope::PathScoped,
        })]);
        let result = resolve_path("git", Some("/tmp"), &reg);
        // /tmp should canonicalize to something (may differ by OS)
        assert!(
            result.is_some(),
            "path-scoped provider should honor explicit path"
        );
    }

    #[test]
    fn unknown_provider_returns_none() {
        let reg = ProviderRegistry::new();
        let result = resolve_path("nonexistent", Some("/tmp"), &reg);
        // Unknown providers have no sources to declare PathScoped, so returns None.
        assert_eq!(result, None);
    }
}
