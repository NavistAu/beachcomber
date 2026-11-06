//! Source-aware request planning: turns a raw request key plus an optional path
//! hint into a resolved `QueryPlan` (provider, effective path, parsed target,
//! and the set of Sources the query demands).
//!
//! This is the single place that understands key syntax. `Get` and `Watch` both
//! build a `QueryPlan` here so they share identical resolution semantics.

use crate::provider::registry::ProviderRegistry;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Check a request key against the one key grammar
/// ([`libbeachcomber::key`]): a head, optionally `name:facet`, then segments
/// written `.seg`, `[n]` or `['…']`. A colon anywhere below the head is part of
/// a segment. A malformed facet in the head is [`parse_facet`]'s error, and a
/// head naming a reserved word is [`libbeachcomber::key::check_head`]'s. Every
/// op that takes a key calls this first.
pub fn validate_key(key: &str) -> Result<(), String> {
    let (head, _) = libbeachcomber::key::split_key(key)?;
    libbeachcomber::key::check_head(head)
}

/// The facet syntax of a head; the rules are [`libbeachcomber::key::parse_facet`]'s.
pub use libbeachcomber::key::parse_facet;

/// Parse a request key into one of two forms. A source name is metadata on a
/// record, never a level in the key tree (`docs/canon/provider_source.md`
/// "Record") — the second-and-later segments are always the field, however
/// many dots it contains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyParse {
    /// "provider"
    Provider(String),
    /// "provider.field" — the address below the head, in canonical form
    /// ([`libbeachcomber::key`]). May have any number of segments:
    /// `provider.field.a.b.c` is a legitimate address once a field's value is
    /// a nested Object (field_resolution.md invariant 12), and
    /// `provider::lookup_path` walks it.
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

/// A key's head and its canonical address: `"provider"` -> `Provider`,
/// anything with an address -> `Field(provider, address)`. A key the grammar
/// refuses is [`validate_key`]'s error.
pub fn parse_key(key: &str) -> Result<KeyParse, String> {
    let (head, address) = libbeachcomber::key::split_key(key)?;
    Ok(if address.is_empty() {
        KeyParse::Provider(head.to_string())
    } else {
        KeyParse::Field(head.to_string(), address)
    })
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
/// A request's path is absolute: the client made it so, against the caller's
/// own directory and `$HOME` (`libbeachcomber::path::normalise`), which the
/// daemon cannot see. A relative path, an empty one or one starting `~` is
/// refused, never joined to the daemon's own working directory. A path that
/// cannot be canonicalised is kept verbatim rather than dropped.
///
/// **The global entry is `$HOME`: its address is `None`, its directory is
/// `$HOME`.** A request naming `$HOME` and a request carrying no path at all
/// are the same request, and both key the pathless entry — so `$HOME` never
/// gets a cache entry of its own beside the global one. The directory half of
/// that rule is [`instance_dir`]: the instance at the global entry still has a
/// real directory to read, and it is `$HOME`.
pub fn normalise_path(requested: Option<&str>) -> Result<Option<String>, String> {
    let Some(p) = requested else {
        return Ok(None);
    };
    if !Path::new(p).is_absolute() {
        return Err(format!("path must be absolute: {p:?}"));
    }
    Ok(address_of(p))
}

/// The cache address of an absolute directory: canonicalised (kept verbatim
/// when it cannot be), and `None` — the global entry — for `$HOME`. What
/// [`normalise_path`] gives a request, and what the daemon's own directories
/// (rendered roots, instance directories) are keyed by.
pub fn address_of(dir: &str) -> Option<String> {
    let absolute = Path::new(dir)
        .canonicalize()
        .map(|abs| abs.to_string_lossy().to_string())
        .unwrap_or_else(|_| dir.to_string());
    if is_home(&absolute) {
        return None;
    }
    Some(absolute)
}

/// Whether `dir` is the daemon's `$HOME`, byte for byte.
pub fn is_home(dir: &str) -> bool {
    home_dir().is_some_and(|home| home.as_os_str() == dir)
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
    /// The chain is over: it ended at the instance at `~`, or, for a nearest
    /// Source, after its one instance or after finding none.
    Done,
    /// The next instance is the one at `~`; no `root` is asked.
    Home,
    /// The next instance is whatever the Source's `root` says for this
    /// directory.
    Root(String),
}

/// What a Source's `root` found for a directory, kept apart so that a walk
/// that found nothing is not mistaken for one that found `$HOME` itself: both
/// are the instance at `~` for a cascading Source, but only the second is an
/// instance for a nearest one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// A root other than `$HOME`.
    At(String),
    /// `$HOME`: the instance at `~`.
    Home,
    /// No root.
    Nothing,
}

/// The walk up one Source's instances from a directory.
///
/// A cascading Source's walk is `R1 = root(dir)`, then `root(parent(R1))`, and
/// so on, ending at the instance at `~` (`None`) and never going past it.
/// Finding nothing continues to `~`. An instance already visited ends the walk
/// at `~` instead of repeating, so a `root` that answers the same directory
/// for every parent still terminates.
///
/// A nearest Source's walk is `[R1]` and no more: finding nothing is no
/// instance, and it starts from `$HOME` when the read is at `~`, since the
/// repository there (or above) is the answer. A Source with no root has the one
/// instance at `~` either way.
///
/// The walk does not call `root` itself: it says what it needs ([`Ask`]) and is
/// given the answer ([`InstanceWalk::step`]), so one walk serves the blocking
/// callers and the request path, which keeps a first render of a config
/// Source's `root` line off the async workers.
#[derive(Debug, Clone)]
pub struct InstanceWalk {
    /// The directory the walk starts from; `None` is the global entry.
    start: Option<String>,
    /// Whether the nearest instance answers alone.
    nearest: bool,
    /// The instances yielded so far, nearest first.
    seen: Vec<Option<String>>,
    /// Set when a nearest walk found nothing.
    over: bool,
}

impl InstanceWalk {
    /// A walk for a read at `dir`: the plan's `requested_path`, so `None` is
    /// `$HOME` or no path at all. `nearest` is the Source's setting and only
    /// counts for a Source with a root.
    pub fn new(dir: Option<&str>, nearest: bool) -> Self {
        Self {
            start: dir.map(str::to_string),
            nearest,
            seen: Vec::new(),
            over: false,
        }
    }

    /// What the next instance needs.
    pub fn ask(&self) -> Ask {
        if self.over {
            return Ask::Done;
        }
        match self.seen.last() {
            None => match (&self.start, self.nearest) {
                (Some(dir), _) => Ask::Root(dir.clone()),
                (None, true) => home_dir().map_or(Ask::Home, |home| {
                    Ask::Root(home.to_string_lossy().into_owned())
                }),
                (None, false) => Ask::Home,
            },
            Some(_) if self.nearest => Ask::Done,
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

    /// Take the answer to [`InstanceWalk::ask`] (ignored for `Ask::Home`) and
    /// return the next instance: `Some(root)`, `Some(None)` for the instance
    /// at `~`, or `None` when a nearest walk found no instance at all.
    pub fn step(&mut self, answer: Found) -> Result<Option<Option<String>>, String> {
        let mut next = match self.ask() {
            Ask::Done => return Err("the instance chain is over".into()),
            Ask::Home => None,
            Ask::Root(_) => match answer {
                Found::At(root) => Some(root),
                Found::Home => None,
                Found::Nothing if self.nearest => {
                    self.over = true;
                    return Ok(None);
                }
                Found::Nothing => None,
            },
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
        Ok(Some(next))
    }
}

impl InstanceWalk {
    /// Walk to the end, nearest first, asking `resolve` what the Source's
    /// `root` finds for each directory. `Ok(None)` when `resolve` returned
    /// `None`, a root it cannot answer yet: the chain is not known. The request
    /// path walks a step at a time instead ([`InstanceWalk::ask`] and
    /// [`InstanceWalk::step`]), since resolving a root there may need to wait.
    pub fn collect(
        mut self,
        mut resolve: impl FnMut(&str) -> Result<Option<Found>, String>,
    ) -> Result<Option<Vec<Option<String>>>, String> {
        let mut chain = Vec::new();
        loop {
            let answer = match self.ask() {
                Ask::Done => return Ok(Some(chain)),
                Ask::Home => Found::Home,
                Ask::Root(dir) => match resolve(&dir)? {
                    Some(found) => found,
                    None => return Ok(None),
                },
            };
            match self.step(answer)? {
                Some(root) => chain.push(root),
                None => return Ok(Some(chain)),
            }
        }
    }
}

/// Every instance of `provider`'s Source `source` that a read at `dir` can
/// reach, nearest first. A cascading Source ends with the instance at `~`
/// (`None`), and one with no root has the one-instance chain `[None]`; a
/// nearest Source has the one instance its root finds, or none.
///
/// `dir` is the plan's `requested_path`. An error is a `root` that could not be
/// worked out; it is the read's error, never the instance at `~`.
pub fn instance_chain(
    registry: &ProviderRegistry,
    provider: &str,
    source: &str,
    dir: Option<&str>,
) -> Result<Vec<Option<String>>, String> {
    registry
        .instance_walk(provider, source, dir)
        .collect(|root_dir| registry.instance_root(provider, source, root_dir).map(Some))
        .map(Option::unwrap_or_default)
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

impl Instance {
    pub fn new(source: &str, root: Option<&str>) -> Self {
        Self {
            source: source.to_string(),
            root: root.map(str::to_string),
        }
    }
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
/// different Sources for the same key. A disabled provider (`enabled = false`)
/// demands none: this is where a read, a watch, a refresh and the scheduler's
/// demand all find the Sources they act on.
pub fn demanded_source_names(
    registry: &ProviderRegistry,
    provider: &str,
    demand: &SourceDemand,
) -> Vec<String> {
    if registry.is_disabled(provider) {
        return Vec::new();
    }
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

/// Split a `:put`, `:source` or `:cache` layer facet off the head of a key:
/// `git:source.branch` is `("git.branch", Source)`. A plain key, or one
/// naming the template, macro or memo facet (a namespace of its own), is
/// returned unchanged with every layer. A head whose facet is not one of the six is
/// [`parse_facet`]'s error, naming the head as written.
pub fn split_layer_facet(
    key: &str,
) -> Result<(std::borrow::Cow<'_, str>, crate::cache::Layers), String> {
    use crate::cache::Layers;
    use libbeachcomber::key::Facet;
    let head = libbeachcomber::key::head_of(key);
    let (base, facet) = libbeachcomber::key::split_facet(head)?;
    let layers = match facet {
        Some(Facet::Put) => Layers::Put,
        Some(Facet::Source) => Layers::Source,
        Some(Facet::Cache) => Layers::Cache,
        None | Some(Facet::Template | Facet::Macro | Facet::Memo) => {
            return Ok((key.into(), Layers::All));
        }
    };
    Ok((format!("{base}{}", &key[head.len()..]).into(), layers))
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
        let (key, layers) = split_layer_facet(raw_key)?;
        let requested_path = normalise_path(path_hint)?;
        let target = parse_key(&key)?;

        let provider = match &target {
            KeyParse::Provider(p) => p.clone(),
            KeyParse::Field(p, _) => p.clone(),
        };

        // The Sources the key reads. The layer facets `:put` and `:cache` warm
        // none of them; a caller that warms checks `layers`.
        let demand = match &target {
            KeyParse::Provider(_) => SourceDemand::All,
            // A provider whose lines are not the shipped ones decides what
            // exists: a renamed or dropped field is no Source's, so a field
            // read demands every Source.
            KeyParse::Field(p, _) if !registry.reads_by_default(p) => SourceDemand::All,
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

    fn parse(key: &str) -> KeyParse {
        parse_key(key).unwrap()
    }

    #[test]
    fn parse_key_recognises_provider() {
        assert!(matches!(parse("git"), KeyParse::Provider(p) if p == "git"));
    }

    #[test]
    fn parse_key_recognises_field() {
        assert!(
            matches!(parse("git.branch"), KeyParse::Field(p, f) if p == "git" && f == "branch")
        );
    }

    #[test]
    fn parse_key_unknown_provider_yields_field() {
        assert!(
            matches!(parse("nope.field"), KeyParse::Field(p, f) if p == "nope" && f == "field")
        );
    }

    #[test]
    fn validate_key_takes_the_key_grammar() {
        for key in [
            "git",
            "git.branch",
            "git:cache.branch",
            "p:template",
            "a.b.c",
            "git.branch:age",
            "mise.cargo:cargo-nextest",
            "mise['go:golang.org/x/tools/gopls']",
            "p.list[0]",
        ] {
            assert_eq!(validate_key(key), Ok(()), "{key}");
        }
        for key in ["p..a", "p['a", "/tmp/x", "~/ws"] {
            assert_eq!(validate_key(key), Err(format!("invalid key: {key}")));
        }
    }

    #[test]
    fn parse_key_canonicalises_the_address() {
        assert!(matches!(parse("p.list[0]"), KeyParse::Field(p, f) if p == "p" && f == "list.0"));
        assert!(
            matches!(parse("mise['go:x.y']"), KeyParse::Field(p, f) if p == "mise" && f == "['go:x.y']")
        );
    }

    #[test]
    fn build_provider_demands_all() {
        let reg = ProviderRegistry::shipped(&std::env::vars().collect());
        let plan = QueryPlan::build("git", None, &reg).unwrap();
        assert!(matches!(plan.demand, SourceDemand::All));
        assert_eq!(plan.provider, "git");
    }

    #[test]
    fn build_field_demands_only_owning_source() {
        let reg = ProviderRegistry::shipped(&std::env::vars().collect());
        // branch is owned by the head source (split from refs).
        let plan = QueryPlan::build("git.branch", None, &reg).unwrap();
        match plan.demand {
            SourceDemand::Sources(v) => assert_eq!(v, vec!["head".to_string()]),
            other => panic!("expected Sources([head]), got {other:?}"),
        }
    }

    #[test]
    fn build_a_nested_key_demands_its_nearest_ancestors_source() {
        let reg = ProviderRegistry::shipped(&std::env::vars().collect());
        let plan = QueryPlan::build("git.branch.x", None, &reg).unwrap();
        match plan.demand {
            SourceDemand::Sources(v) => assert_eq!(v, vec!["head".to_string()]),
            other => panic!("expected Sources([head]), got {other:?}"),
        }
    }

    #[test]
    fn build_an_undeclared_field_under_a_sentinel_source_demands_it() {
        let reg = ProviderRegistry::shipped(&std::env::vars().collect());
        let plan = QueryPlan::build("mise.node", None, &reg).unwrap();
        match plan.demand {
            SourceDemand::Sources(v) => assert_eq!(v, vec!["tools".to_string()]),
            other => panic!("expected Sources([tools]), got {other:?}"),
        }
    }

    #[test]
    fn build_an_undeclared_field_without_a_sentinel_demands_nothing() {
        let reg = ProviderRegistry::shipped(&std::env::vars().collect());
        let plan = QueryPlan::build("hostname.nonesuch", None, &reg).unwrap();
        match plan.demand {
            SourceDemand::Sources(v) => assert!(v.is_empty(), "no sentinel, nothing declared"),
            other => panic!("expected Sources([]), got {other:?}"),
        }
    }

    #[test]
    fn build_unknown_field_demands_nothing() {
        let reg = ProviderRegistry::shipped(&std::env::vars().collect());
        let plan = QueryPlan::build("git.totally_bogus_field", None, &reg).unwrap();
        match plan.demand {
            SourceDemand::Sources(v) => assert!(v.is_empty(), "unknown field warms nothing"),
            other => panic!("expected Sources([]), got {other:?}"),
        }
    }

    #[test]
    fn a_layer_facet_splits_off_a_bracketed_key() {
        let (key, layers) = split_layer_facet("mise:cache['go:x.y']").unwrap();
        assert_eq!(key, "mise['go:x.y']");
        assert_eq!(layers, crate::cache::Layers::Cache);
    }

    /// The template and macro facets are namespaces of their own, read whole.
    #[test]
    fn the_template_and_macro_facets_are_plain_namespaces() {
        for key in ["scanline:template.rate", "scanline:macro.rate"] {
            let (split, layers) = split_layer_facet(key).unwrap();
            assert_eq!(split, key);
            assert_eq!(layers, crate::cache::Layers::All);
        }
    }

    #[test]
    fn build_refuses_a_facet_outside_the_closed_set_naming_the_head_as_written() {
        let reg = ProviderRegistry::shipped(&std::env::vars().collect());
        for (key, head) in [
            ("p:notes.x", "p:notes"),
            ("p:bogus:put.x", "p:bogus:put"),
            ("p:bogus:source", "p:bogus:source"),
            ("p:bogus:cache.x", "p:bogus:cache"),
        ] {
            let err = QueryPlan::build(key, None, &reg).unwrap_err();
            assert_eq!(
                err,
                format!(
                    "invalid facet key: {head} (a facet is template, macro, memo, put, source or cache)"
                ),
                "{key}"
            );
        }
    }

    #[test]
    fn build_reads_a_colon_below_the_head_as_part_of_a_segment() {
        let reg = ProviderRegistry::shipped(&std::env::vars().collect());
        let plan = QueryPlan::build("git.branch:fresh", None, &reg).unwrap();
        assert!(matches!(plan.target, KeyParse::Field(_, ref f) if f == "branch:fresh"));
    }
}

#[cfg(test)]
mod instance_chain_tests {
    use super::*;
    use crate::provider::registry::ProviderRegistry;
    use crate::provider::{
        FailbackConfig, FieldSchema, InvalidationStrategy, Source, SourceMetadata, SourceResult,
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
        nearest: bool,
    }
    impl FakeProvider {
        fn meta(&self) -> SourceMetadata {
            SourceMetadata {
                name: "main".into(),
                fields: vec![FieldSchema { name: "v".into() }],
                invalidation: InvalidationStrategy::Poll {
                    interval_secs: 30,
                    count: 2,
                },
                failback: FailbackConfig {
                    reattempts: 3,
                    interval_secs: 30,
                },
                decay: None,
                budget: crate::scheduler::lifecycle::DEFAULT_BUDGET,
                timeout: crate::scheduler::lifecycle::DEFAULT_TIMEOUT,
                nearest: self.nearest,
            }
        }
    }
    impl FakeProvider {
        fn sources(&self) -> Vec<Box<dyn Source>> {
            vec![Box::new(FakeSource {
                meta: self.meta(),
                root_fn: self.root_fn.clone(),
            })]
        }
    }

    fn registry_with(name: &str, root_fn: Option<RootFn>) -> ProviderRegistry {
        registry_of(name, root_fn, false)
    }

    fn registry_of(name: &str, root_fn: Option<RootFn>, nearest: bool) -> ProviderRegistry {
        let mut reg = ProviderRegistry::new();
        let fake = FakeProvider {
            name: name.into(),
            root_fn,
            nearest,
        };
        reg.register_sources(&fake.name, fake.sources()).unwrap();
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
    fn a_nearest_chain_is_the_one_root_found() {
        let outer = format!("{}/ws/outer", home());
        let inner = format!("{outer}/vendor/inner");
        let reg = registry_of("p", Some(marker_walk(vec![outer, inner.clone()])), true);
        let dir = format!("{inner}/src");
        assert_eq!(
            instance_chain(&reg, "p", "main", Some(&dir)).unwrap(),
            vec![Some(inner)],
            "no outer instance, and no instance at ~"
        );
    }

    #[test]
    fn a_nearest_chain_that_found_nothing_is_empty_not_home() {
        let reg = registry_of("p", Some(marker_walk(vec![])), true);
        let dir = format!("{}/ws/proj", home());
        assert_eq!(
            instance_chain(&reg, "p", "main", Some(&dir)).unwrap(),
            Vec::<Option<String>>::new()
        );
        assert_eq!(
            instance_chain(&reg, "p", "main", None).unwrap(),
            Vec::<Option<String>>::new(),
            "a read at ~ asks the root from $HOME, and it found nothing"
        );
    }

    #[test]
    fn a_nearest_chain_that_found_home_is_the_instance_at_home() {
        let reg = registry_of("p", Some(Arc::new(|_dir: &str| Ok(Some(home())))), true);
        let dir = format!("{}/ws/proj", home());
        assert_eq!(
            instance_chain(&reg, "p", "main", Some(&dir)).unwrap(),
            vec![None]
        );
        assert_eq!(instance_chain(&reg, "p", "main", None).unwrap(), vec![None]);
    }

    #[test]
    fn a_nearest_source_with_no_root_still_has_its_instance_at_home() {
        let reg = registry_of("p", None, true);
        let dir = format!("{}/ws/proj", home());
        assert_eq!(
            instance_chain(&reg, "p", "main", Some(&dir)).unwrap(),
            vec![None]
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
        assert_eq!(normalise_path(Some(&home())), Ok(None));
    }

    #[test]
    fn normalise_passes_a_missing_path_through() {
        assert_eq!(normalise_path(None), Ok(None));
        // A path that cannot be canonicalised is kept verbatim rather than dropped.
        assert_eq!(
            normalise_path(Some("/no/such/beachcomber/dir")),
            Ok(Some("/no/such/beachcomber/dir".to_string()))
        );
    }

    /// The daemon never resolves a path against its own working directory:
    /// a path the client did not make absolute is refused.
    #[test]
    fn normalise_refuses_a_path_that_is_not_absolute() {
        for p in ["", ".", "rel/x", "~", "~/x"] {
            assert_eq!(
                normalise_path(Some(p)),
                Err(format!("path must be absolute: {p:?}")),
                "{p:?}"
            );
        }
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
