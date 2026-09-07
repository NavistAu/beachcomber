//! Source-aware request planning: turns a raw request key plus an optional path
//! hint into a resolved `QueryPlan` (provider, effective path, parsed target,
//! metadata suffix, and the set of Sources the query demands).
//!
//! This is the single place that understands key syntax. `Get` and `Watch` both
//! build a `QueryPlan` here so they share identical resolution semantics.

use crate::provider::SourceScope;
use crate::provider::registry::ProviderRegistry;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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

impl KeyParse {
    /// The address below the namespace: the field, or `""` for a
    /// whole-namespace read. Both halves of a read — the data and the template
    /// — are addressed by it.
    pub fn rest(&self) -> &str {
        match self {
            KeyParse::Field(_, field) => field.as_str(),
            KeyParse::Provider(_) => "",
        }
    }
}

/// Splits a key on its first dot: `"provider"` -> `Provider`, anything with a
/// dot -> `Field(provider, rest)`.
pub fn parse_key(key: &str) -> KeyParse {
    match key.split_once('.') {
        Some((p, rest)) => KeyParse::Field(p.to_string(), rest.to_string()),
        None => KeyParse::Provider(key.to_string()),
    }
}

/// The daemon's `$HOME`, canonicalised, read once per process.
///
/// `$HOME` is the global path (canon `field_resolution.md` §"Path resolution"):
/// a request addressing it addresses the global cache entry, which is keyed
/// pathless internally.
pub fn home_dir() -> Option<PathBuf> {
    static HOME: OnceLock<Option<PathBuf>> = OnceLock::new();
    HOME.get_or_init(|| {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|p| p.canonicalize().unwrap_or(p))
    })
    .clone()
}

/// Canonicalise a requested path into the address a cache entry is keyed by.
///
/// A relative path resolves against the daemon's cwd; a path that cannot be
/// canonicalised is kept verbatim rather than dropped.
///
/// **The global entry is `$HOME`: its address is `None`, its directory is
/// `$HOME`.** A request naming `$HOME` and a request carrying no path at all
/// are the same request, and both key the pathless entry — so `$HOME` never
/// gets a cache entry of its own beside the global one. The directory half of
/// that rule is [`source_dir`]: a `PathScoped` Source addressed at the global
/// entry still has a real directory to read, and it is `$HOME`.
pub fn normalise_path(requested: Option<&str>) -> Option<String> {
    let p = requested?;
    let path = Path::new(p);
    let absolute = if path.is_relative() {
        std::env::current_dir()
            .ok()
            .and_then(|cwd| cwd.join(path).canonicalize().ok())
            .map(|abs| abs.to_string_lossy().to_string())
            .unwrap_or_else(|| p.to_string())
    } else {
        path.canonicalize()
            .map(|abs| abs.to_string_lossy().to_string())
            .unwrap_or_else(|_| p.to_string())
    };
    if home_dir().is_some_and(|home| home.as_os_str() == absolute.as_str()) {
        return None;
    }
    Some(absolute)
}

/// The directory a Source runs in to produce the records at cache address
/// `address` — the directory half of the rule at [`normalise_path`].
///
/// Everywhere but one case this is the address itself. The exception is a
/// `PathScoped` Source at the global entry, whose address is `None` and whose
/// directory is therefore `$HOME`: without it a repository at `$HOME` would be
/// queried with no directory at all and answer nothing.
pub fn source_dir(scope: SourceScope, address: Option<&str>) -> Option<String> {
    match (scope, address) {
        (SourceScope::PathScoped, None) => {
            home_dir().map(|home| home.to_string_lossy().into_owned())
        }
        _ => address.map(str::to_string),
    }
}

/// The chain of cache-entry addresses a read at `path` consults, nearest first
/// and always ending at the global entry.
///
/// `path` itself, then each ancestor below `$HOME` when `path` is under
/// `$HOME` and every ancestor up to `/` when it is not, then `None` — the
/// global entry. A pathless read has the one-link chain `[None]`.
///
/// `$HOME` itself is never a link: it *is* the global entry, and
/// [`normalise_path`] turns every address naming it into `None`, so no writer
/// can put anything at `Some($HOME)` for the walk to find.
pub fn path_chain(path: Option<&str>) -> Vec<Option<String>> {
    let Some(p) = path else {
        return vec![None];
    };
    let home = home_dir();
    let mut out = Vec::new();
    let mut current = Some(Path::new(p));
    while let Some(dir) = current {
        if home.as_deref() == Some(dir) {
            break;
        }
        out.push(Some(dir.to_string_lossy().into_owned()));
        current = dir.parent();
    }
    out.push(None);
    out
}

/// Every cache-entry address one read of a namespace consults, nearest first:
/// [`path_chain`] from the requested path, plus `provider_entry` as the head
/// when it is not already a link.
///
/// `provider_entry` — [`QueryPlan::effective_path`] — is normally on that chain
/// already: a `canonical_path` that walks to a project root returns an
/// ancestor-or-self of the requested path, or `$HOME`, which [`normalise_path`]
/// turns into the global link. An env-selected file source (`kubecontext`,
/// `talos`) is the exception: its address is a `:`-joined list of files, no
/// directory and no ancestor of one.
///
/// [`Cache::read_merged`] walks this, and `watch` subscribes it, so a write to
/// any entry a frame is assembled from wakes the stream.
///
/// [`Cache::read_merged`]: crate::cache::Cache::read_merged
/// [`QueryPlan::effective_path`]: QueryPlan::effective_path
pub fn read_chain(requested: Option<&str>, provider_entry: Option<&str>) -> Vec<Option<String>> {
    let mut chain = path_chain(requested);
    if !chain.iter().any(|layer| layer.as_deref() == provider_entry) {
        chain.insert(0, provider_entry.map(str::to_string));
    }
    chain
}

/// Resolve the effective path for a request: the cache entry the request
/// addresses, and the head of the chain a read walks.
///
/// `requested` is the request's own path, already canonicalised by
/// [`normalise_path`] — the one canonicalisation a request pays for, since the
/// caller keeps that value too (a template resolves from it, while the data
/// half resolves from what this returns).
///
/// What differs between namespaces is scope: a namespace that declares no
/// sources — a static one, or an unknown name — has no scope of its own, so the
/// requested path is its address; a provider whose sources are all `Global`
/// declares that it has one answer, which lives at the global entry; and a
/// provider with a `PathScoped` source may narrow the path further through that
/// source's `canonical_path` (walking to a project root, say).
fn resolve_path(key: &str, requested: Option<&str>, registry: &ProviderRegistry) -> Option<String> {
    let provider = key.split('.').next().unwrap_or(key);
    let raw = requested?;

    let Some(sources) = registry.provider_sources(provider) else {
        return Some(raw.to_string());
    };
    if !sources.iter().any(|s| s.scope == SourceScope::PathScoped) {
        return None;
    }
    for sm in sources {
        if sm.scope == SourceScope::PathScoped
            && let Some(src) = registry.source(provider, &sm.name)
            && let Some(canonical) = src.canonical_path(Some(raw))
        {
            // A walk that stayed put returns an already-canonical path. One
            // that moved has to be normalised, because it can land on `$HOME`
            // — a repository rooted there, queried from a subdirectory — and
            // `$HOME` is the global entry. Otherwise the same directory would
            // key two entries: `Some($HOME)` from the walk and `None` from a
            // direct request.
            return if canonical == raw {
                Some(canonical)
            } else {
                normalise_path(Some(&canonical))
            };
        }
    }
    Some(raw.to_string())
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

impl SourceDemand {
    /// Whether the Source named `name` is one this query demands. The one
    /// place the two variants are distinguished — every caller that filters a
    /// provider's Sources goes through here, so `All` and `Sources` cannot
    /// drift apart between the server and the scheduler.
    pub fn selects(&self, name: &str) -> bool {
        match self {
            SourceDemand::All => true,
            SourceDemand::Sources(names) => names.iter().any(|n| n == name),
        }
    }
}

/// The names of `provider`'s registered Sources that `demand` selects, in
/// declaration order. Empty when the provider declares none — a static
/// namespace, a facet, or an unknown name — and when no declared Source
/// answers the demand, which is what a field no Source owns produces.
///
/// `get` warms this set and `refresh` re-executes it, so the two cannot select
/// different Sources for the same key.
pub fn demanded_source_names(
    registry: &ProviderRegistry,
    provider: &str,
    demand: &SourceDemand,
) -> Vec<String> {
    registry
        .provider_sources(provider)
        .map(|sources| {
            sources
                .iter()
                .filter(|sm| demand.selects(&sm.name))
                .map(|sm| sm.name.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// A resolved, source-aware request. Built once per request and consumed by
/// both `Get` and `Watch` so they share identical resolution semantics.
#[derive(Debug, Clone)]
pub struct QueryPlan {
    pub provider: String,
    /// The request's own path, canonicalised ([`normalise_path`]) — where the
    /// caller is standing. Static records, and so the template half of a read,
    /// resolve up the chain from here.
    pub requested_path: Option<String>,
    /// Where the *provider's* records live: [`requested_path`] narrowed by the
    /// namespace's scope. The data half of a read is addressed here.
    ///
    /// [`requested_path`]: QueryPlan::requested_path
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
        let requested_path = normalise_path(path_hint);
        let effective_path = resolve_path(stripped_key, requested_path.as_deref(), registry);
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
            requested_path,
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
        let result = resolve_path("hostname", normalise_path(Some("/tmp")).as_deref(), &reg);
        assert_eq!(result, None, "global provider must ignore explicit path");
    }

    #[test]
    fn path_scoped_provider_honors_explicit_path() {
        let reg = registry_with(vec![Box::new(FakeProvider {
            name: "git".into(),
            scope: SourceScope::PathScoped,
        })]);
        let result = resolve_path("git", normalise_path(Some("/tmp")).as_deref(), &reg);
        // /tmp should canonicalize to something (may differ by OS)
        assert!(
            result.is_some(),
            "path-scoped provider should honor explicit path"
        );
    }

    #[test]
    fn a_namespace_with_no_sources_keeps_the_requested_path() {
        let reg = ProviderRegistry::new();
        // A namespace that declares no sources — a static one, or an unknown
        // name — has no scope of its own, so the requested path is its path and
        // the chain resolves the rest.
        let result = resolve_path("nonexistent", normalise_path(Some("/tmp")).as_deref(), &reg);
        assert_eq!(result, normalise_path(Some("/tmp")));
        assert!(result.is_some(), "/tmp is not $HOME");
    }

    #[test]
    fn a_request_addressing_home_resolves_to_the_global_entry() {
        let reg = ProviderRegistry::new();
        let home = home_dir().expect("$HOME is set");
        assert_eq!(
            resolve_path(
                "anything",
                normalise_path(Some(&home.to_string_lossy())).as_deref(),
                &reg
            ),
            None,
            "$HOME is the global path"
        );
    }
}

#[cfg(test)]
mod path_chain_tests {
    use super::*;

    fn home() -> String {
        home_dir()
            .expect("$HOME is set")
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn normalise_maps_home_to_the_global_entry() {
        assert_eq!(normalise_path(Some(&home())), None);
    }

    #[test]
    fn normalise_passes_a_missing_path_through() {
        assert_eq!(normalise_path(None), None);
        // A path that cannot be canonicalised is kept verbatim rather than dropped.
        assert_eq!(
            normalise_path(Some("/no/such/beachcomber/dir")),
            Some("/no/such/beachcomber/dir".to_string())
        );
    }

    #[test]
    fn chain_of_the_global_entry_is_just_itself() {
        assert_eq!(path_chain(None), vec![None]);
    }

    #[test]
    fn chain_under_home_stops_below_home() {
        let home = home();
        let deep = format!("{home}/ws/project/foo");
        assert_eq!(
            path_chain(Some(&deep)),
            vec![
                Some(deep.clone()),
                Some(format!("{home}/ws/project")),
                Some(format!("{home}/ws")),
                None,
            ],
            "$HOME is the global entry, so it is the `None` link and not a `Some` one"
        );
    }

    #[test]
    fn chain_of_home_itself_is_only_the_global_entry() {
        assert_eq!(path_chain(Some(&home())), vec![None]);
    }

    #[test]
    fn chain_outside_home_walks_to_the_root() {
        // A top-level directory cannot be under $HOME unless $HOME is `/`.
        let chain = path_chain(Some("/no-such-beachcomber-root/a"));
        assert_eq!(
            chain,
            vec![
                Some("/no-such-beachcomber-root/a".to_string()),
                Some("/no-such-beachcomber-root".to_string()),
                Some("/".to_string()),
                None,
            ]
        );
    }
}
