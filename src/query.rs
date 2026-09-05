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

/// Parse the facet syntax of a provider segment. A provider name containing a
/// colon is a facet key: `base:facet` (see `docs/canon/field_resolution.md`
/// "Facet"). Returns `Ok(None)` for a plain provider name (no colon),
/// `Ok(Some(facet))` for a syntactically valid facet key, or `Err` for a
/// malformed one — more than one colon, an empty base or facet, invalid
/// characters, or a facet name that collides with a metadata suffix (those
/// five names are unreachable as facets; a suffix strip always claims them
/// first).
pub fn parse_facet(provider: &str) -> Result<Option<&str>, String> {
    if !provider.contains(':') {
        return Ok(None);
    }
    let (base, facet) = provider.split_once(':').expect("checked contains(':')");
    let valid = !base.is_empty()
        && !facet.is_empty()
        && facet
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && !KNOWN_METADATA_SUFFIXES.contains(&facet);
    if valid {
        Ok(Some(facet))
    } else {
        Err(format!("invalid facet key: {provider}"))
    }
}

/// Parse a request key into one of two forms. A source name is metadata on a
/// record, never a level in the key tree (`docs/canon/provider_source.md`
/// "Record") — the second-and-later segments are always the field, however
/// many dots it contains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyParse {
    /// "provider"
    Provider(String),
    /// "provider.field" — everything after the first dot, verbatim. May
    /// itself contain dots: `provider.field.a.b.c` is a legitimate address
    /// once a field's value is a nested Object (field_resolution.md
    /// invariant 12); `provider::lookup_path` walks the remainder.
    Field(String, String),
}

/// Splits a key on its first dot: `"provider"` -> `Provider`, anything with a
/// dot -> `Field(provider, rest)`.
pub fn parse_key(key: &str) -> KeyParse {
    match key.split_once('.') {
        Some((p, rest)) => KeyParse::Field(p.to_string(), rest.to_string()),
        None => KeyParse::Provider(key.to_string()),
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

    // Static namespaces don't declare sources, but they can be stored with a path.
    // Pass the raw path through (canonicalized) so path-keyed static data is accessible.
    if registry.is_static_namespace(provider) {
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
        let target = parse_key(stripped_key);

        let provider = match &target {
            KeyParse::Provider(p) => p.clone(),
            KeyParse::Field(p, _) => p.clone(),
        };

        let demand = match &target {
            KeyParse::Provider(_) => SourceDemand::All,
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
        assert!(matches!(parse_key("git"), KeyParse::Provider(p) if p == "git"));
    }

    #[test]
    fn parse_key_source_name_is_just_a_field() {
        // "refs" is a registered source name for git, but a source name is
        // never part of an address (docs/canon/provider_source.md "Record"):
        // git.refs parses as a field like any other.
        assert!(
            matches!(parse_key("git.refs"), KeyParse::Field(p, f) if p == "git" && f == "refs")
        );
    }

    #[test]
    fn parse_key_recognises_field() {
        assert!(
            matches!(parse_key("git.branch"), KeyParse::Field(p, f) if p == "git" && f == "branch")
        );
    }

    #[test]
    fn parse_key_source_name_prefix_is_a_dotted_field() {
        // Same rule for three segments: "refs" is not consulted as a source
        // name, so the whole remainder is the field.
        assert!(
            matches!(parse_key("git.refs.branch"), KeyParse::Field(p, f) if p == "git" && f == "refs.branch")
        );
    }

    #[test]
    fn parse_key_unknown_provider_yields_field() {
        assert!(
            matches!(parse_key("nope.field"), KeyParse::Field(p, f) if p == "nope" && f == "field")
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
        // branch is owned by the head source (split from refs).
        let plan = QueryPlan::build("git.branch", None, &reg);
        match plan.demand {
            SourceDemand::Sources(v) => assert_eq!(v, vec!["head".to_string()]),
            other => panic!("expected Sources([head]), got {other:?}"),
        }
    }

    #[test]
    fn build_source_name_as_field_demands_nothing() {
        let reg = ProviderRegistry::with_defaults();
        // "refs" is a source name, not a field: git.refs parses as Field, and
        // no field owns it, so it demands nothing (same as any unknown field).
        let plan = QueryPlan::build("git.refs", None, &reg);
        assert!(matches!(plan.target, KeyParse::Field(_, ref f) if f == "refs"));
        match plan.demand {
            SourceDemand::Sources(v) => assert!(v.is_empty(), "git.refs is not a field"),
            other => panic!("expected Sources([]), got {other:?}"),
        }
    }

    #[test]
    fn build_source_name_prefixed_field_demands_nothing() {
        let reg = ProviderRegistry::with_defaults();
        let plan = QueryPlan::build("git.refs.branch", None, &reg);
        match plan.demand {
            SourceDemand::Sources(v) => assert!(v.is_empty(), "git.refs.branch addresses no field"),
            other => panic!("expected Sources([]), got {other:?}"),
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
    fn parse_facet_none_when_no_colon() {
        assert_eq!(parse_facet("myapp"), Ok(None));
    }

    #[test]
    fn parse_facet_valid_single_colon() {
        assert_eq!(parse_facet("scanline:template"), Ok(Some("template")));
    }

    #[test]
    fn parse_facet_allows_underscores_hyphens_and_digits() {
        assert_eq!(parse_facet("a:my_facet-2"), Ok(Some("my_facet-2")));
    }

    #[test]
    fn parse_facet_rejects_two_colons() {
        assert!(parse_facet("a:b:c").is_err());
    }

    #[test]
    fn parse_facet_rejects_empty_base_or_facet() {
        assert!(parse_facet(":facet").is_err());
        assert!(parse_facet("base:").is_err());
    }

    #[test]
    fn parse_facet_rejects_invalid_characters() {
        assert!(parse_facet("a:b!").is_err());
    }

    #[test]
    fn parse_facet_rejects_known_metadata_suffix_names() {
        for suffix in KNOWN_METADATA_SUFFIXES {
            assert!(
                parse_facet(&format!("a:{suffix}")).is_err(),
                "facet named after metadata suffix '{suffix}' must be rejected"
            );
        }
    }

    #[test]
    fn build_strips_metadata_suffix() {
        let reg = ProviderRegistry::with_defaults();
        let plan = QueryPlan::build("git.branch:fresh", None, &reg);
        assert_eq!(plan.meta.as_deref(), Some("fresh"));
        match plan.demand {
            SourceDemand::Sources(v) => assert_eq!(v, vec!["head".to_string()]),
            other => panic!("expected Sources([head]), got {other:?}"),
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
