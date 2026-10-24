//! Source-aware request planning: turns a raw request key plus an optional path
//! hint into a resolved `QueryPlan` (provider, effective path, parsed target,
//! and the set of Sources the query demands).
//!
//! This is the single place that understands key syntax. `Get` and `Watch` both
//! build a `QueryPlan` here so they share identical resolution semantics.

use crate::provider::registry::ProviderRegistry;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Check the colons of a request key. One colon form exists: `p:facet`, with
/// the colon in the first segment, before any dot. A colon after the first dot
/// makes the key invalid; a malformed facet in the first segment is
/// [`parse_facet`]'s error. Every op that takes a key calls this first.
pub fn validate_key(key: &str) -> Result<(), String> {
    if key
        .split_once('.')
        .is_some_and(|(_, rest)| rest.contains(':'))
    {
        return Err(format!("invalid key: {key}"));
    }
    Ok(())
}

/// Parse the facet syntax of a provider segment. A provider name containing a
/// colon is a facet key: `base:facet` (see `docs/canon/field_resolution.md`
/// "Facet"). Returns `Ok(None)` for a plain provider name (no colon),
/// `Ok(Some(facet))` for a syntactically valid facet key, or `Err` for a
/// malformed one — more than one colon, an empty base or facet, invalid
/// characters, or a facet name that is `put` or `source`, the layer facets a
/// request key splits off before this check and a config table cannot be.
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
        && !matches!(facet, "put" | "source");
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
/// that rule is [`instance_dir`]: the instance at the global entry still has a
/// real directory to read, and it is `$HOME`.
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

/// The directory a Source runs in to produce the records of the instance at
/// `root` — the directory half of the rule at [`normalise_path`].
///
/// The instance's root, except at the instance at `~`, whose address is `None`
/// and whose directory is `$HOME`: without it a repository at `$HOME` would be
/// queried with no directory at all and answer nothing.
pub fn instance_dir(root: Option<&str>) -> Option<String> {
    match root {
        Some(root) => Some(root.to_string()),
        None => home_dir().map(|home| home.to_string_lossy().into_owned()),
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
///
/// This is the chain static records resolve up. The chain a read of a
/// namespace actually walks is [`read_chain`], which also carries the entry
/// the namespace's own records live at.
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
/// [`path_chain`] from the requested path, plus each instance the read reached
/// that is not already a link of it.
///
/// A reached root is usually on that chain already: a Source's `root` walks up
/// from the requested path, so it is an ancestor-or-self of it, or `$HOME`,
/// which [`normalise_path`] turns into the global link. But the walk does not
/// stop at `$HOME` and the chain does: a project marker *above* `$HOME` (a
/// repository at `/srv/x` with `$HOME` at `/srv/x/home`) is an instance at a
/// directory the chain from `$HOME/proj` never reaches. The instance still has
/// to be read and watched, so it is added here, nearest of the extras first.
///
/// [`Cache::read_merged`] walks this, and `watch` subscribes it, so a write to
/// any entry a frame is assembled from wakes the stream.
///
/// [`Cache::read_merged`]: crate::cache::Cache::read_merged
pub fn read_chain(requested: Option<&str>, extra: &[Option<String>]) -> Vec<Option<String>> {
    let on_chain = path_chain(requested);
    let mut chain: Vec<Option<String>> = Vec::new();
    for root in extra {
        if !on_chain.contains(root) && !chain.contains(root) {
            chain.push(root.clone());
        }
    }
    chain.extend(on_chain);
    chain
}

/// How many instances one chain may have before it is taken for a Source whose
/// `root` never settles. A real chain has one per directory level at most.
const MAX_INSTANCES: usize = 64;

/// What the next instance of a chain needs to be found.
#[derive(Debug, PartialEq, Eq)]
pub enum Ask {
    /// The chain is over: it ended at the instance at `~`.
    Done,
    /// The next instance is the one at `~`; no `root` is asked.
    Home,
    /// The next instance is whatever the Source's `root` says for this
    /// directory.
    Root(String),
}

/// The walk up one Source's instances from a directory: `R1 = root(dir)`, then
/// `root(parent(R1))`, and so on, ending at the instance at `~` (`None`) and
/// never going past it. An instance already visited ends the walk at `~`
/// instead of repeating, so a `root` that answers the same directory for every
/// parent still terminates.
///
/// The walk does not call `root` itself: it says what it needs ([`Ask`]) and is
/// given the answer ([`InstanceWalk::step`]), so one walk serves the blocking
/// callers and the request path, which keeps a first render of a config
/// Source's `root` line off the async workers.
#[derive(Debug, Clone)]
pub struct InstanceWalk {
    /// The directory the walk starts from; `None` is the global entry.
    start: Option<String>,
    /// The instances yielded so far, nearest first.
    seen: Vec<Option<String>>,
}

impl InstanceWalk {
    /// A walk for a read at `dir`: the plan's `requested_path`, so `None` is
    /// `$HOME` or no path at all, whose only instance is the one at `~`.
    pub fn new(dir: Option<&str>) -> Self {
        Self {
            start: dir.map(str::to_string),
            seen: Vec::new(),
        }
    }

    /// What the next instance needs.
    pub fn ask(&self) -> Ask {
        match self.seen.last() {
            None => match &self.start {
                Some(dir) => Ask::Root(dir.clone()),
                None => Ask::Home,
            },
            Some(None) => Ask::Done,
            Some(Some(root)) => match Path::new(root).parent() {
                // The filesystem root has no parent, and `$HOME` is the
                // instance at `~` itself: either way the next is `~`.
                None => Ask::Home,
                Some(parent) if home_dir().as_deref() == Some(parent) => Ask::Home,
                Some(parent) => Ask::Root(parent.to_string_lossy().into_owned()),
            },
        }
    }

    /// Take the answer to [`InstanceWalk::ask`] (`root`'s normalised answer for
    /// `Ask::Root`, ignored for `Ask::Home`) and return the next instance:
    /// `Some(root)` or `None` for the instance at `~`.
    pub fn step(&mut self, answer: Option<String>) -> Result<Option<String>, String> {
        let mut next = match self.ask() {
            Ask::Done => return Err("the instance chain is over".into()),
            Ask::Home => None,
            Ask::Root(_) => answer,
        };
        if next.is_some() && self.seen.contains(&next) {
            next = None;
        }
        if self.seen.len() >= MAX_INSTANCES {
            return Err(format!(
                "more than {MAX_INSTANCES} instances up the chain: root does not settle"
            ));
        }
        self.seen.push(next.clone());
        Ok(next)
    }
}

/// Every instance of `provider`'s Source `source` that a read at `dir` can
/// reach, nearest first, ending with the instance at `~` (`None`). A Source
/// whose `root` is always `None` has the one-instance chain `[None]`.
///
/// `dir` is the plan's `requested_path`. An error is a `root` that could not be
/// worked out; it is the read's error, never the instance at `~`.
pub fn instance_chain(
    registry: &ProviderRegistry,
    provider: &str,
    source: &str,
    dir: Option<&str>,
) -> Result<Vec<Option<String>>, String> {
    let mut walk = InstanceWalk::new(dir);
    let mut chain = Vec::new();
    loop {
        let answer = match walk.ask() {
            Ask::Done => return Ok(chain),
            Ask::Home => None,
            Ask::Root(dir) => registry.instance_root(provider, source, &dir)?,
        };
        chain.push(walk.step(answer)?);
    }
}

/// Every instance of every Source `demand` selects, for a read at `dir`:
/// `(source, root)` pairs, each Source's chain nearest first.
pub fn instances_for(
    registry: &ProviderRegistry,
    provider: &str,
    demand: &SourceDemand,
    dir: Option<&str>,
) -> Result<Vec<Instance>, String> {
    let mut out = Vec::new();
    for source in demanded_source_names(registry, provider, demand) {
        for root in instance_chain(registry, provider, &source, dir)? {
            out.push(Instance {
                source: source.clone(),
                root,
            });
        }
    }
    Ok(out)
}

/// One instance of a Source: the Source's name and the root it answers for
/// (`None` is the instance at `~`). The unit demand and refreshes address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    pub source: String,
    pub root: Option<String>,
}

/// The distinct roots of `instances`, in the order first met: the extra links
/// [`read_chain`] adds to a read.
pub fn roots_of(instances: &[Instance]) -> Vec<Option<String>> {
    let mut roots: Vec<Option<String>> = Vec::new();
    for instance in instances {
        if !roots.contains(&instance.root) {
            roots.push(instance.root.clone());
        }
    }
    roots
}

/// Which Sources a query demands warming for.
///
/// An explicit enum keeps field→source resolution in this layer while the
/// instances each Source is warmed at are found per Source ([`InstanceWalk`]).
/// `Sources(vec![])` means "warm nothing" (e.g. an unknown field) and is
/// deliberately distinct from `All`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceDemand {
    /// Whole-provider query: warm every Source.
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
    /// resolve up the chain from here, and it is where each demanded Source's
    /// instance chain starts ([`InstanceWalk`]).
    pub requested_path: Option<String>,
    pub target: KeyParse,
    pub demand: SourceDemand,
    /// Which layers the read sees: `p:put` and `p:source` name one, anything
    /// else sees them all. `p:cache` reads them all to report on them.
    pub layers: crate::cache::Layers,
}

/// Split a `:put`, `:source` or `:cache` layer facet off the provider segment
/// of a key: `git:source.branch` is `("git.branch", Source)`. Any other key is
/// returned unchanged with every layer.
pub fn split_layer_facet(key: &str) -> (std::borrow::Cow<'_, str>, crate::cache::Layers) {
    use crate::cache::Layers;
    let (segment, rest) = match key.find('.') {
        Some(i) => key.split_at(i),
        None => (key, ""),
    };
    for (suffix, layers) in [
        (":put", Layers::Put),
        (":source", Layers::Source),
        (":cache", Layers::Cache),
    ] {
        if let Some(base) = segment.strip_suffix(suffix)
            && !base.is_empty()
        {
            return (format!("{base}{rest}").into(), layers);
        }
    }
    (key.into(), Layers::All)
}

impl QueryPlan {
    /// Resolve a raw request key + optional path hint into a plan, or refuse
    /// a key whose colons are invalid ([`validate_key`]).
    pub fn build(
        raw_key: &str,
        path_hint: Option<&str>,
        registry: &ProviderRegistry,
    ) -> Result<QueryPlan, String> {
        validate_key(raw_key)?;
        let (key, layers) = split_layer_facet(raw_key);
        let requested_path = normalise_path(path_hint);
        let target = parse_key(&key);

        let provider = match &target {
            KeyParse::Provider(p) => p.clone(),
            KeyParse::Field(p, _) => p.clone(),
        };

        // The Sources the key reads. The layer facets `:put` and `:cache` warm
        // none of them; a caller that warms checks `layers`.
        let demand = match &target {
            KeyParse::Provider(_) => SourceDemand::All,
            KeyParse::Field(p, f) => {
                // A field's demand is the Source of its nearest ancestor node,
                // else the provider's sentinel Sources, else nothing.
                SourceDemand::Sources(
                    registry
                        .sources_for_key(p, f)
                        .into_iter()
                        .map(String::from)
                        .collect(),
                )
            }
        };

        Ok(QueryPlan {
            provider,
            requested_path,
            target,
            demand,
            layers,
        })
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
    fn validate_key_allows_one_colon_in_the_first_segment() {
        for key in [
            "git",
            "git.branch",
            "git:cache.branch",
            "p:template",
            "a.b.c",
        ] {
            assert_eq!(validate_key(key), Ok(()), "{key}");
        }
    }

    #[test]
    fn validate_key_refuses_a_colon_after_the_first_segment() {
        for key in ["git.branch:age", "git:cache.x:stale", "a.b:c"] {
            assert_eq!(validate_key(key), Err(format!("invalid key: {key}")));
        }
    }

    #[test]
    fn build_provider_demands_all() {
        let reg = ProviderRegistry::with_defaults();
        let plan = QueryPlan::build("git", None, &reg).unwrap();
        assert!(matches!(plan.demand, SourceDemand::All));
        assert_eq!(plan.provider, "git");
    }

    #[test]
    fn build_field_demands_only_owning_source() {
        let reg = ProviderRegistry::with_defaults();
        // branch is owned by the head source (split from refs).
        let plan = QueryPlan::build("git.branch", None, &reg).unwrap();
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
        let plan = QueryPlan::build("git.refs", None, &reg).unwrap();
        assert!(matches!(plan.target, KeyParse::Field(_, ref f) if f == "refs"));
        match plan.demand {
            SourceDemand::Sources(v) => assert!(v.is_empty(), "git.refs is not a field"),
            other => panic!("expected Sources([]), got {other:?}"),
        }
    }

    #[test]
    fn build_source_name_prefixed_field_demands_nothing() {
        let reg = ProviderRegistry::with_defaults();
        let plan = QueryPlan::build("git.refs.branch", None, &reg).unwrap();
        match plan.demand {
            SourceDemand::Sources(v) => assert!(v.is_empty(), "git.refs.branch addresses no field"),
            other => panic!("expected Sources([]), got {other:?}"),
        }
    }

    #[test]
    fn build_a_nested_key_demands_its_nearest_ancestors_source() {
        let reg = ProviderRegistry::with_defaults();
        let plan = QueryPlan::build("git.branch.x", None, &reg).unwrap();
        match plan.demand {
            SourceDemand::Sources(v) => assert_eq!(v, vec!["head".to_string()]),
            other => panic!("expected Sources([head]), got {other:?}"),
        }
    }

    #[test]
    fn build_an_undeclared_field_under_a_sentinel_source_demands_it() {
        let reg = ProviderRegistry::with_defaults();
        let plan = QueryPlan::build("mise.node", None, &reg).unwrap();
        match plan.demand {
            SourceDemand::Sources(v) => assert_eq!(v, vec!["tools".to_string()]),
            other => panic!("expected Sources([tools]), got {other:?}"),
        }
    }

    #[test]
    fn build_an_undeclared_field_without_a_sentinel_demands_nothing() {
        let reg = ProviderRegistry::with_defaults();
        let plan = QueryPlan::build("hostname.nonesuch", None, &reg).unwrap();
        match plan.demand {
            SourceDemand::Sources(v) => assert!(v.is_empty(), "no sentinel, nothing declared"),
            other => panic!("expected Sources([]), got {other:?}"),
        }
    }

    #[test]
    fn build_unknown_field_demands_nothing() {
        let reg = ProviderRegistry::with_defaults();
        let plan = QueryPlan::build("git.totally_bogus_field", None, &reg).unwrap();
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
    fn parse_facet_rejects_the_layer_facet_names() {
        for facet in ["put", "source"] {
            assert!(parse_facet(&format!("a:{facet}")).is_err(), "{facet}");
        }
    }

    #[test]
    fn build_refuses_a_colon_outside_the_provider_segment() {
        let reg = ProviderRegistry::with_defaults();
        let err = QueryPlan::build("git.branch:fresh", None, &reg).unwrap_err();
        assert_eq!(err, "invalid key: git.branch:fresh");
    }
}

#[cfg(test)]
mod instance_chain_tests {
    use super::*;
    use crate::provider::registry::ProviderRegistry;
    use crate::provider::{
        FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
        Source, SourceMetadata, SourceResult,
    };
    use std::sync::Arc;

    type RootFn = Arc<dyn Fn(&str) -> Result<Option<String>, String> + Send + Sync>;

    /// A Source whose root is whatever `root_fn` says, or none at all.
    struct FakeSource {
        meta: SourceMetadata,
        root_fn: Option<RootFn>,
    }
    impl Source for FakeSource {
        fn metadata(&self) -> &SourceMetadata {
            &self.meta
        }
        fn execute(&self, _dir: Option<&str>) -> Result<SourceResult, String> {
            Ok(SourceResult::new())
        }
        fn root(&self, dir: &str) -> Result<Option<String>, String> {
            match &self.root_fn {
                Some(f) => f(dir),
                None => Ok(None),
            }
        }
        fn has_root(&self) -> bool {
            self.root_fn.is_some()
        }
    }

    struct FakeProvider {
        name: String,
        root_fn: Option<RootFn>,
    }
    impl FakeProvider {
        fn meta(&self) -> SourceMetadata {
            SourceMetadata {
                name: "main".into(),
                fields: vec![FieldSchema {
                    name: "v".into(),
                    field_type: FieldType::String,
                }],
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
    }
    impl Provider for FakeProvider {
        fn metadata(&self) -> ProviderMetadata {
            ProviderMetadata {
                name: self.name.clone(),
                sources: vec![self.meta()],
            }
        }
        fn sources(&self) -> Vec<Box<dyn Source>> {
            vec![Box::new(FakeSource {
                meta: self.meta(),
                root_fn: self.root_fn.clone(),
            })]
        }
    }

    fn registry_with(name: &str, root_fn: Option<RootFn>) -> ProviderRegistry {
        let mut reg = ProviderRegistry::new();
        reg.register(Box::new(FakeProvider {
            name: name.into(),
            root_fn,
        }))
        .unwrap();
        reg
    }

    fn home() -> String {
        home_dir()
            .expect("$HOME is set")
            .to_string_lossy()
            .into_owned()
    }

    /// A root that is the nearest of `markers` at or above the directory: the
    /// walk a project marker does, without touching the filesystem.
    fn marker_walk(markers: Vec<String>) -> RootFn {
        Arc::new(move |dir: &str| {
            Ok(markers
                .iter()
                .filter(|m| Path::new(dir).starts_with(m.as_str()))
                .max_by_key(|m| m.len())
                .cloned())
        })
    }

    #[test]
    fn instance_chain_ends_at_home() {
        let proj = format!("{}/ws/proj", home());
        let reg = registry_with("p", Some(marker_walk(vec![proj.clone()])));
        let dir = format!("{proj}/src");
        assert_eq!(
            instance_chain(&reg, "p", "main", Some(&dir)).unwrap(),
            vec![Some(proj), None]
        );
    }

    #[test]
    fn instance_chain_climbs_through_nested_roots() {
        let outer = format!("{}/ws/outer", home());
        let inner = format!("{outer}/vendor/inner");
        let reg = registry_with("p", Some(marker_walk(vec![outer.clone(), inner.clone()])));
        let dir = format!("{inner}/src");
        assert_eq!(
            instance_chain(&reg, "p", "main", Some(&dir)).unwrap(),
            vec![Some(inner), Some(outer), None]
        );
    }

    #[test]
    fn instance_chain_skips_repeated_roots() {
        // A root that answers the same directory whatever it is asked about:
        // the parent of that root asks again, gets it again, and the walk ends
        // at `~` instead of going round.
        let fixed = format!("{}/ws/fixed", home());
        let answer = fixed.clone();
        let reg = registry_with(
            "p",
            Some(Arc::new(move |_dir: &str| Ok(Some(answer.clone())))),
        );
        let dir = format!("{}/ws/elsewhere/deep", home());
        assert_eq!(
            instance_chain(&reg, "p", "main", Some(&dir)).unwrap(),
            vec![Some(fixed), None]
        );
    }

    #[test]
    fn a_source_with_no_root_has_one_instance() {
        let reg = registry_with("p", None);
        let dir = format!("{}/ws/anywhere", home());
        assert_eq!(
            instance_chain(&reg, "p", "main", Some(&dir)).unwrap(),
            vec![None]
        );
        assert_eq!(instance_chain(&reg, "p", "main", None).unwrap(), vec![None]);
    }

    #[test]
    fn a_root_that_is_home_is_the_instance_at_home() {
        let reg = registry_with("p", Some(Arc::new(|_dir: &str| Ok(Some(home())))));
        let dir = format!("{}/ws/proj", home());
        assert_eq!(
            instance_chain(&reg, "p", "main", Some(&dir)).unwrap(),
            vec![None]
        );
    }

    #[test]
    fn a_root_above_home_is_walked_past_it_to_home() {
        // `$HOME`'s parent is not home, so the walk continues up from the
        // marker and still ends at `~`.
        let above = "/no-such-beachcomber-marker/repo".to_string();
        let reg = registry_with("p", Some(marker_walk(vec![above.clone()])));
        let dir = format!("{above}/deep/er");
        assert_eq!(
            instance_chain(&reg, "p", "main", Some(&dir)).unwrap(),
            vec![Some(above), None]
        );
    }

    #[test]
    fn a_root_that_fails_is_the_chains_error_not_home() {
        let reg = registry_with("p", Some(Arc::new(|_dir: &str| Err("no luck".to_string()))));
        let dir = format!("{}/ws/proj", home());
        assert_eq!(
            instance_chain(&reg, "p", "main", Some(&dir)),
            Err("no luck".to_string())
        );
    }

    #[test]
    fn a_root_that_never_settles_is_an_error() {
        // Every ask names a root the walk has not seen: a new one beside the
        // last.
        let asked = std::sync::atomic::AtomicUsize::new(0);
        let reg = registry_with(
            "p",
            Some(Arc::new(move |_dir: &str| {
                let n = asked.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(Some(format!("/no-such-beachcomber-root/{n}")))
            })),
        );
        let dir = format!("{}/ws/proj", home());
        let err = instance_chain(&reg, "p", "main", Some(&dir)).unwrap_err();
        assert!(err.contains("does not settle"), "{err}");
    }

    #[test]
    fn read_chain_adds_a_root_the_directory_chain_does_not_reach() {
        let above = Some("/no-such-beachcomber-marker/repo".to_string());
        let dir = format!("{}/ws/proj", home());
        let chain = read_chain(Some(&dir), &[above.clone(), None]);
        assert_eq!(chain.first(), Some(&above), "the extra link leads");
        assert_eq!(chain.last(), Some(&None), "the global entry ends it");
        assert_eq!(
            chain.iter().filter(|l| l.is_none()).count(),
            1,
            "a root already on the chain is not added twice: {chain:?}"
        );
    }

    #[test]
    fn instance_dir_is_home_for_the_instance_at_home() {
        assert_eq!(instance_dir(None), Some(home()));
        assert_eq!(instance_dir(Some("/a/b")), Some("/a/b".to_string()));
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
