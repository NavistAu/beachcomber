//! Source-aware request planning: turns a raw request key plus an optional path
//! hint into a resolved `QueryPlan` (provider, effective path, parsed target,
//! and the set of Sources the query demands).
//!
//! The key grammar is [`libbeachcomber::key`]'s; a request key is parsed once,
//! here, into a [`Key`]. `Get` and `Watch` both build a `QueryPlan` here so
//! they share identical resolution semantics.

use crate::provider::registry::ProviderRegistry;
use std::path::Path;

/// The facet syntax of a head; the rules are [`libbeachcomber::key::parse_facet`]'s.
pub use libbeachcomber::key::parse_facet;

/// A request key, parsed once: the provider it reads and the address below
/// it, as segments and in canonical form ([`libbeachcomber::key`]). A source
/// name is metadata on a record, never a level in the key tree
/// (`website/docs/internals/architecture.md`, "Provider→Source layering"), so
/// everything below the head is the address, however many segments it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    /// The head with any layer facet split off (`git:source.branch` reads
    /// `git`); a template, macro or memo facet stays, a namespace of its own.
    pub provider: String,
    /// The address's segments: empty for a whole-namespace read.
    pub path: Vec<String>,
    /// `path` in canonical form, which records, memos and the template
    /// half are keyed by.
    address: String,
}

impl Key {
    /// Check `key` against the one key grammar and parse it, with the layers
    /// its facet names: a head, optionally `name:facet`, then segments
    /// written `.seg`, `[n]` or `['…']`. A colon below the head is part of a
    /// segment. Refused, in this order: what the grammar refuses
    /// ([`libbeachcomber::key::split_key`]), a head naming a reserved word
    /// ([`libbeachcomber::key::check_head`]), and a facet that is not one of
    /// the six ([`parse_facet`], naming the head as written). `:put`,
    /// `:source` and `:cache` name a layer; `p:cache` reads them all to
    /// report on them.
    pub fn parse(key: &str) -> Result<(Key, crate::cache::Layers), String> {
        use crate::cache::Layers;
        use libbeachcomber::key::Facet;
        let (head, path) = libbeachcomber::key::parse_key(key)?;
        libbeachcomber::key::check_head(head)?;
        let (base, facet) = libbeachcomber::key::split_facet(head)?;
        let (provider, layers) = match facet {
            Some(Facet::Put) => (base, Layers::Put),
            Some(Facet::Source) => (base, Layers::Source),
            Some(Facet::Cache) => (base, Layers::Cache),
            None | Some(Facet::Template | Facet::Macro | Facet::Memo) => (head, Layers::All),
        };
        let key = Key {
            provider: provider.to_string(),
            address: libbeachcomber::key::format_address(&path),
            path,
        };
        Ok((key, layers))
    }

    /// The address below the namespace in canonical form, or `""` for a
    /// whole-namespace read.
    pub fn rest(&self) -> &str {
        &self.address
    }

    /// Whether the key addresses a node below the namespace.
    pub fn is_field(&self) -> bool {
        !self.path.is_empty()
    }
}

pub use libbeachcomber::path::Home;

/// A cache entry's directory: the global entry, or an absolute directory.
///
/// **The global entry is `$HOME`: its address is `None`, its directory is
/// `$HOME`.** A request naming `$HOME` and a request carrying no path at all
/// are the same request, and both key the pathless entry — so `$HOME` never
/// gets a cache entry of its own beside the global one. The directory half of
/// that rule is [`Dir::path`]: the instance at the global entry still has a
/// real directory to read, and it is `$HOME`.
///
/// Whose `$HOME` is the caller's to say: a daemon's is its registry's
/// ([`ProviderRegistry::home`]), captured once from the environment it was
/// given, so nothing below reads the process environment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Dir {
    /// The global entry, the instance at `~`.
    Global,
    /// An absolute directory other than `$HOME`.
    At(String),
}

impl Dir {
    /// The entry `dir` names as written: `$HOME` is the global one.
    pub fn of(dir: &str, home: &Home) -> Dir {
        match home.dir() {
            Some(h) if Path::new(h) == Path::new(dir) => Dir::Global,
            _ => Dir::At(dir.to_string()),
        }
    }

    /// The entry a cache address names: `None` is the global entry. For an
    /// address already made ([`Dir::address`]), as a cache key holds it.
    pub fn keyed(address: Option<&str>) -> Dir {
        address.map_or(Dir::Global, |a| Dir::At(a.to_string()))
    }

    /// The entry a daemon read at `dir` is keyed by ([`Dir::canonical`]);
    /// no directory is the global entry.
    pub fn named(dir: Option<&str>, home: &Home) -> Dir {
        dir.map_or(Dir::Global, |dir| Dir::canonical(dir, home))
    }

    /// The entry an absolute directory is keyed by: canonicalised (kept
    /// verbatim when it cannot be), then [`Dir::of`]. What a request's path
    /// becomes ([`Dir::requested`]), and what the daemon's own directories
    /// (rendered roots, instance directories) are keyed by.
    pub fn canonical(dir: &str, home: &Home) -> Dir {
        let absolute = Path::new(dir)
            .canonicalize()
            .map(|abs| abs.to_string_lossy().to_string())
            .unwrap_or_else(|_| dir.to_string());
        Dir::of(&absolute, home)
    }

    /// The entry a request's path names; no path is the global entry.
    ///
    /// A request's path is absolute: the client made it so, against the
    /// caller's own directory and `$HOME` (`libbeachcomber::path::normalise`),
    /// which the daemon cannot see. A relative path, an empty one or one
    /// starting `~` is refused, never joined to the daemon's own working
    /// directory. A path that cannot be canonicalised is kept verbatim rather
    /// than dropped ([`Dir::canonical`]).
    pub fn requested(requested: Option<&str>, home: &Home) -> Result<Dir, String> {
        let Some(p) = requested else {
            return Ok(Dir::Global);
        };
        if !Path::new(p).is_absolute() {
            return Err(format!("path must be absolute: {p:?}"));
        }
        Ok(Dir::canonical(p, home))
    }

    /// The address a cache entry is keyed by: `None` for the global entry.
    pub fn address(&self) -> Option<&str> {
        match self {
            Dir::Global => None,
            Dir::At(dir) => Some(dir),
        }
    }

    /// [`Dir::address`], owned.
    pub fn into_address(self) -> Option<String> {
        match self {
            Dir::Global => None,
            Dir::At(dir) => Some(dir),
        }
    }

    /// Whether this is the global entry.
    pub fn is_global(&self) -> bool {
        *self == Dir::Global
    }

    /// The directory a Source runs in to produce the records of the instance
    /// here: the directory itself, except at the global entry, whose directory
    /// is `$HOME` — without it a repository at `$HOME` would be queried with
    /// no directory at all and answer nothing.
    pub fn path<'a>(&'a self, home: &'a Home) -> Option<&'a str> {
        match self {
            Dir::Global => home.dir(),
            Dir::At(dir) => Some(dir),
        }
    }

    /// The entry above this one: the parent directory, else the global entry
    /// — above the filesystem root, and at a child of `$HOME`, whose parent
    /// *is* the global entry. The global entry is its own parent.
    pub fn parent(&self, home: &Home) -> Dir {
        match self {
            Dir::Global => Dir::Global,
            Dir::At(dir) => Path::new(dir)
                .parent()
                .map_or(Dir::Global, |up| Dir::of(&up.to_string_lossy(), home)),
        }
    }

    /// The chain of entries a read here consults, nearest first and always
    /// ending at the global entry.
    ///
    /// This directory, then each ancestor below `$HOME` when it is under
    /// `$HOME` and every ancestor up to `/` when it is not, then the global
    /// entry. A read at the global entry has the one-link chain `[Global]`.
    ///
    /// `$HOME` itself is never a link: it *is* the global entry, and
    /// [`Dir::of`] turns every directory naming it into [`Dir::Global`], so no
    /// writer can put anything at `$HOME`'s own address for the walk to find.
    ///
    /// This is the chain static records resolve up. The chain a read of a
    /// namespace actually walks is [`read_chain`], which also carries the
    /// entry the namespace's own records live at.
    pub fn chain(&self, home: &Home) -> Vec<Dir> {
        let mut at = match self {
            Dir::At(dir) => Dir::of(dir, home),
            Dir::Global => Dir::Global,
        };
        let mut out = Vec::new();
        while !at.is_global() {
            let up = at.parent(home);
            out.push(at);
            at = up;
        }
        out.push(Dir::Global);
        out
    }
}

/// Every cache-entry address one read of a namespace consults, nearest first:
/// [`Dir::chain`] from the requested path, plus each instance the read reached
/// that is not already a link of it.
///
/// A reached root is usually on that chain already: a Source's `root` walks up
/// from the requested path, so it is an ancestor-or-self of it, or `$HOME`,
/// which [`Dir::of`] turns into the global link. But the walk does not
/// stop at `$HOME` and the chain does: a project marker *above* `$HOME` (a
/// repository at `/srv/x` with `$HOME` at `/srv/x/home`) is an instance at a
/// directory the chain from `$HOME/proj` never reaches. The instance still has
/// to be read and watched, so it is added here, nearest of the extras first.
///
/// [`Cache::read_merged`] walks this, and `watch` subscribes it, so a write to
/// any entry a frame is assembled from wakes the stream.
///
/// [`Cache::read_merged`]: crate::cache::Cache::read_merged
pub fn read_chain(requested: &Dir, extra: &[Dir], home: &Home) -> Vec<Dir> {
    let on_chain = requested.chain(home);
    let mut chain: Vec<Dir> = Vec::new();
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

impl From<Dir> for Found {
    /// A root found at `dir`: [`Found::Home`] at the global entry.
    fn from(dir: Dir) -> Found {
        match dir {
            Dir::Global => Found::Home,
            Dir::At(root) => Found::At(root),
        }
    }
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
    /// The entry the walk starts from.
    start: Dir,
    /// Whether the nearest instance answers alone.
    nearest: bool,
    /// The instances yielded so far, nearest first.
    seen: Vec<Dir>,
    /// Set when a nearest walk found nothing.
    over: bool,
    /// The `$HOME` the global entry is.
    home: Home,
}

impl InstanceWalk {
    /// A walk for a read at `dir`: the plan's `requested_path`, so the global
    /// entry is `$HOME` or no path at all. `nearest` is the Source's setting
    /// and only counts for a Source with a root.
    pub fn new(dir: &Dir, nearest: bool, home: &Home) -> Self {
        Self {
            start: dir.clone(),
            nearest,
            seen: Vec::new(),
            over: false,
            home: home.clone(),
        }
    }

    /// What the next instance needs.
    pub fn ask(&self) -> Ask {
        if self.over {
            return Ask::Done;
        }
        match self.seen.last() {
            None => match (&self.start, self.nearest) {
                (Dir::At(dir), _) => Ask::Root(dir.clone()),
                (Dir::Global, true) => self
                    .home
                    .dir()
                    .map_or(Ask::Home, |home| Ask::Root(home.to_string())),
                (Dir::Global, false) => Ask::Home,
            },
            Some(_) if self.nearest => Ask::Done,
            Some(Dir::Global) => Ask::Done,
            // Above the filesystem root, and above a child of `$HOME`, is the
            // instance at `~`.
            Some(root) => match root.parent(&self.home) {
                Dir::Global => Ask::Home,
                Dir::At(parent) => Ask::Root(parent),
            },
        }
    }

    /// Take the answer to [`InstanceWalk::ask`] (ignored for `Ask::Home`) and
    /// return the next instance, or `None` when a nearest walk found no
    /// instance at all.
    pub fn step(&mut self, answer: Found) -> Result<Option<Dir>, String> {
        let mut next = match self.ask() {
            Ask::Done => return Err("the instance chain is over".into()),
            Ask::Home => Dir::Global,
            Ask::Root(_) => match answer {
                Found::At(root) => Dir::At(root),
                Found::Home => Dir::Global,
                Found::Nothing if self.nearest => {
                    self.over = true;
                    return Ok(None);
                }
                Found::Nothing => Dir::Global,
            },
        };
        if !next.is_global() && self.seen.contains(&next) {
            next = Dir::Global;
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
    ) -> Result<Option<Vec<Dir>>, String> {
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
/// ([`Dir::Global`]), and one with no root has the one-instance chain
/// `[Global]`; a nearest Source has the one instance its root finds, or none.
///
/// `dir` is the plan's `requested_path`. An error is a `root` that could not be
/// worked out; it is the read's error, never the instance at `~`.
pub fn instance_chain(
    registry: &ProviderRegistry,
    provider: &str,
    source: &str,
    dir: &Dir,
) -> Result<Vec<Dir>, String> {
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
    dir: &Dir,
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

/// One instance of a Source: the Source's name and the root it answers for.
/// The unit demand and refreshes address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    pub source: String,
    pub root: Dir,
}

impl Instance {
    /// The instance of `source` at the cache address `root` (`None` is the
    /// instance at `~`).
    pub fn new(source: &str, root: Option<&str>) -> Self {
        Self {
            source: source.to_string(),
            root: Dir::keyed(root),
        }
    }
}

/// The distinct roots of `instances`, in the order first met: the extra links
/// [`read_chain`] adds to a read.
pub fn roots_of(instances: &[Instance]) -> Vec<Dir> {
    let mut roots: Vec<Dir> = Vec::new();
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
/// demands none, and a disabled Source is never demanded: this is where a
/// read, a watch, a refresh and the scheduler's demand all find the Sources
/// they act on.
pub fn demanded_source_names(
    registry: &ProviderRegistry,
    provider: &str,
    demand: &SourceDemand,
) -> Vec<String> {
    let mut names = selected_source_names(registry, provider, demand);
    names.retain(|s| !registry.is_source_disabled(provider, s));
    names
}

/// The Sources, as `p.s`, that alone serve `demand` and are switched off:
/// `None` when any Source it selects is on, or none is selected. What a
/// refresh or a `--force` of the key refuses with.
pub fn only_disabled_sources(
    registry: &ProviderRegistry,
    provider: &str,
    demand: &SourceDemand,
) -> Option<String> {
    let selected = selected_source_names(registry, provider, demand);
    (!selected.is_empty()
        && selected
            .iter()
            .all(|s| registry.is_source_disabled(provider, s)))
    .then(|| {
        selected
            .iter()
            .map(|s| format!("{provider}.{s}"))
            .collect::<Vec<_>>()
            .join(", ")
    })
}

/// The names of `provider`'s Sources `demand` selects, disabled ones too:
/// what tells a key only a disabled Source serves from one none does.
pub fn selected_source_names(
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
    /// The request's own path, canonicalised ([`Dir::requested`]) — where the
    /// caller is standing. Static records, and so the template half of a read,
    /// resolve up the chain from here, and it is where each demanded Source's
    /// instance chain starts ([`InstanceWalk`]).
    pub requested_path: Dir,
    pub target: Key,
    pub demand: SourceDemand,
    /// Which layers the read sees: `p:put` and `p:source` name one, anything
    /// else sees them all. `p:cache` reads them all to report on them.
    pub layers: crate::cache::Layers,
}

impl QueryPlan {
    /// Resolve a raw request key + optional path hint into a plan, or refuse
    /// a key the grammar refuses ([`Key::parse`]).
    pub fn build(
        raw_key: &str,
        path_hint: Option<&str>,
        registry: &ProviderRegistry,
    ) -> Result<QueryPlan, String> {
        let (target, layers) = Key::parse(raw_key)?;
        let requested_path = Dir::requested(path_hint, registry.home())?;
        let provider = target.provider.clone();

        // The Sources the key reads. The layer facets `:put` and `:cache` warm
        // none of them; a caller that warms checks `layers`.
        let demand = if !target.is_field() {
            SourceDemand::All
        } else if !registry.reads_by_default(&provider) {
            // A provider whose lines are not the shipped ones decides what
            // exists: a renamed or dropped field is no Source's, so a field
            // read demands every Source.
            SourceDemand::All
        } else {
            // A field's demand is the Source of its nearest ancestor node,
            // else the provider's sentinel Sources, else nothing.
            SourceDemand::Sources(
                registry
                    .sources_for_path(&provider, &target.path)
                    .into_iter()
                    .map(String::from)
                    .collect(),
            )
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

    /// A key's provider and canonical address.
    fn parse(key: &str) -> (String, String) {
        let (key, _) = Key::parse(key).unwrap();
        (key.provider.clone(), key.rest().to_string())
    }

    fn owned(p: &str, f: &str) -> (String, String) {
        (p.to_string(), f.to_string())
    }

    #[test]
    fn parse_key_recognises_provider() {
        let (key, _) = Key::parse("git").unwrap();
        assert!(!key.is_field());
        assert_eq!(parse("git"), owned("git", ""));
    }

    #[test]
    fn parse_key_recognises_field() {
        assert_eq!(parse("git.branch"), owned("git", "branch"));
    }

    #[test]
    fn parse_key_unknown_provider_yields_field() {
        assert_eq!(parse("nope.field"), owned("nope", "field"));
    }

    #[test]
    fn parse_key_keeps_the_segments_and_their_canonical_form() {
        let (key, _) = Key::parse("p.a['b.c'][0]").unwrap();
        assert_eq!(key.path, vec!["a", "b.c", "0"]);
        assert_eq!(key.rest(), "a['b.c'].0");
    }

    #[test]
    fn key_parse_takes_the_key_grammar() {
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
            assert!(Key::parse(key).is_ok(), "{key}");
        }
        for key in ["p..a", "p['a", "/tmp/x", "~/ws"] {
            assert_eq!(Key::parse(key), Err(format!("invalid key: {key}")));
        }
    }

    #[test]
    fn parse_key_canonicalises_the_address() {
        assert_eq!(parse("p.list[0]"), owned("p", "list.0"));
        assert_eq!(parse("mise['go:x.y']"), owned("mise", "['go:x.y']"));
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
        let (key, layers) = Key::parse("mise:cache['go:x.y']").unwrap();
        assert_eq!((key.provider.as_str(), key.rest()), ("mise", "['go:x.y']"));
        assert_eq!(layers, crate::cache::Layers::Cache);
    }

    /// The template and macro facets are namespaces of their own, read whole.
    #[test]
    fn the_template_and_macro_facets_are_plain_namespaces() {
        for (key, head) in [
            ("scanline:template.rate", "scanline:template"),
            ("scanline:macro.rate", "scanline:macro"),
        ] {
            let (split, layers) = Key::parse(key).unwrap();
            assert_eq!((split.provider.as_str(), split.rest()), (head, "rate"));
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
        assert_eq!(plan.target.rest(), "branch:fresh");
    }
}

#[cfg(test)]
mod instance_chain_tests {
    use super::*;
    use crate::provider::registry::ProviderRegistry;
    use crate::provider::{
        FailbackConfig, FieldSchema, Schedule, Source, SourceMetadata, SourceResult,
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
                schedule: Schedule {
                    failback: FailbackConfig {
                        reattempts: 3,
                        interval_secs: 30,
                    },
                    nearest: self.nearest,
                    ..Schedule::poll(30, 2)
                },
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

    /// The `$HOME` these registries are given: no real directory, so no test
    /// here depends on the one it runs under.
    const HOME: &str = "/no-such-beachcomber-home/u";

    fn registry_of(name: &str, root_fn: Option<RootFn>, nearest: bool) -> ProviderRegistry {
        let mut reg = ProviderRegistry::new().with_home(Home::of(Some(HOME)));
        let fake = FakeProvider {
            name: name.into(),
            root_fn,
            nearest,
        };
        reg.register_sources(&fake.name, fake.sources()).unwrap();
        reg
    }

    fn home() -> String {
        HOME.to_string()
    }

    /// The instance chain of `reg`'s `p.main` from `dir` (`None` is `~`), as
    /// cache addresses.
    fn chain_at(reg: &ProviderRegistry, dir: Option<&str>) -> Result<Vec<Option<String>>, String> {
        let dir = dir.map_or(Dir::Global, |d| Dir::of(d, reg.home()));
        instance_chain(reg, "p", "main", &dir)
            .map(|chain| chain.into_iter().map(Dir::into_address).collect())
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
        assert_eq!(chain_at(&reg, Some(&dir)).unwrap(), vec![Some(proj), None]);
    }

    #[test]
    fn instance_chain_climbs_through_nested_roots() {
        let outer = format!("{}/ws/outer", home());
        let inner = format!("{outer}/vendor/inner");
        let reg = registry_with("p", Some(marker_walk(vec![outer.clone(), inner.clone()])));
        let dir = format!("{inner}/src");
        assert_eq!(
            chain_at(&reg, Some(&dir)).unwrap(),
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
        assert_eq!(chain_at(&reg, Some(&dir)).unwrap(), vec![Some(fixed), None]);
    }

    #[test]
    fn a_source_with_no_root_has_one_instance() {
        let reg = registry_with("p", None);
        let dir = format!("{}/ws/anywhere", home());
        assert_eq!(chain_at(&reg, Some(&dir)).unwrap(), vec![None]);
        assert_eq!(chain_at(&reg, None).unwrap(), vec![None]);
    }

    #[test]
    fn a_root_that_is_home_is_the_instance_at_home() {
        let reg = registry_with("p", Some(Arc::new(|_dir: &str| Ok(Some(home())))));
        let dir = format!("{}/ws/proj", home());
        assert_eq!(chain_at(&reg, Some(&dir)).unwrap(), vec![None]);
    }

    #[test]
    fn a_root_above_home_is_walked_past_it_to_home() {
        // `$HOME`'s parent is not home, so the walk continues up from the
        // marker and still ends at `~`.
        let above = "/no-such-beachcomber-marker/repo".to_string();
        let reg = registry_with("p", Some(marker_walk(vec![above.clone()])));
        let dir = format!("{above}/deep/er");
        assert_eq!(chain_at(&reg, Some(&dir)).unwrap(), vec![Some(above), None]);
    }

    #[test]
    fn a_nearest_chain_is_the_one_root_found() {
        let outer = format!("{}/ws/outer", home());
        let inner = format!("{outer}/vendor/inner");
        let reg = registry_of("p", Some(marker_walk(vec![outer, inner.clone()])), true);
        let dir = format!("{inner}/src");
        assert_eq!(
            chain_at(&reg, Some(&dir)).unwrap(),
            vec![Some(inner)],
            "no outer instance, and no instance at ~"
        );
    }

    #[test]
    fn a_nearest_chain_that_found_nothing_is_empty_not_home() {
        let reg = registry_of("p", Some(marker_walk(vec![])), true);
        let dir = format!("{}/ws/proj", home());
        assert_eq!(
            chain_at(&reg, Some(&dir)).unwrap(),
            Vec::<Option<String>>::new()
        );
        assert_eq!(
            chain_at(&reg, None).unwrap(),
            Vec::<Option<String>>::new(),
            "a read at ~ asks the root from $HOME, and it found nothing"
        );
    }

    #[test]
    fn a_nearest_chain_that_found_home_is_the_instance_at_home() {
        let reg = registry_of("p", Some(Arc::new(|_dir: &str| Ok(Some(home())))), true);
        let dir = format!("{}/ws/proj", home());
        assert_eq!(chain_at(&reg, Some(&dir)).unwrap(), vec![None]);
        assert_eq!(chain_at(&reg, None).unwrap(), vec![None]);
    }

    #[test]
    fn a_nearest_source_with_no_root_still_has_its_instance_at_home() {
        let reg = registry_of("p", None, true);
        let dir = format!("{}/ws/proj", home());
        assert_eq!(chain_at(&reg, Some(&dir)).unwrap(), vec![None]);
    }

    #[test]
    fn a_root_that_fails_is_the_chains_error_not_home() {
        let reg = registry_with("p", Some(Arc::new(|_dir: &str| Err("no luck".to_string()))));
        let dir = format!("{}/ws/proj", home());
        assert_eq!(chain_at(&reg, Some(&dir)), Err("no luck".to_string()));
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
        let err = chain_at(&reg, Some(&dir)).unwrap_err();
        assert!(err.contains("does not settle"), "{err}");
    }

    #[test]
    fn read_chain_adds_a_root_the_directory_chain_does_not_reach() {
        let above = Dir::At("/no-such-beachcomber-marker/repo".to_string());
        let dir = Dir::At(format!("{}/ws/proj", home()));
        let chain = read_chain(&dir, &[above.clone(), Dir::Global], &Home::of(Some(HOME)));
        assert_eq!(chain.first(), Some(&above), "the extra link leads");
        assert_eq!(chain.last(), Some(&Dir::Global), "the global entry ends it");
        assert_eq!(
            chain.iter().filter(|l| l.is_global()).count(),
            1,
            "a root already on the chain is not added twice: {chain:?}"
        );
    }

    #[test]
    fn the_directory_of_the_global_entry_is_home() {
        let home = Home::of(Some(HOME));
        assert_eq!(Dir::Global.path(&home), Some(HOME));
        assert_eq!(Dir::At("/a/b".into()).path(&home), Some("/a/b"));
        assert_eq!(
            Dir::Global.path(&Home::default()),
            None,
            "no home, no directory"
        );
    }
}

#[cfg(test)]
mod path_chain_tests {
    use super::*;

    /// A `$HOME` that is a real directory, so canonicalising reaches it.
    fn real_home() -> (tempfile::TempDir, Home) {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::of(Some(&tmp.path().to_string_lossy()));
        (tmp, home)
    }

    const HOME: &str = "/no-such-beachcomber-home/u";

    fn home() -> Home {
        Home::of(Some(HOME))
    }

    /// The chain from `dir` (`None` is `~`), as cache addresses.
    fn path_chain(dir: Option<&str>) -> Vec<Option<String>> {
        let home = home();
        dir.map_or(Dir::Global, |d| Dir::of(d, &home))
            .chain(&home)
            .into_iter()
            .map(Dir::into_address)
            .collect()
    }

    fn normalise_path(requested: Option<&str>) -> Result<Option<String>, String> {
        Dir::requested(requested, &home()).map(Dir::into_address)
    }

    #[test]
    fn normalise_maps_home_to_the_global_entry() {
        assert_eq!(normalise_path(Some(HOME)), Ok(None));
        // `$HOME` as written, through a symlink (macOS `/tmp`), is the global
        // entry too: both sides are canonical.
        let (tmp, home) = real_home();
        assert_eq!(
            Dir::requested(Some(&tmp.path().to_string_lossy()), &home),
            Ok(Dir::Global)
        );
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

    /// The same requested path, asked again as the tree changes under it,
    /// always gets what realpath says now: a memo of the answer must not
    /// outlive a rename, a removal or a directory replaced by a symlink.
    #[test]
    fn normalise_follows_the_tree_as_it_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let s = |p: &std::path::Path| p.to_string_lossy().into_owned();
        let ask = |p: &std::path::Path| normalise_path(Some(&s(p))).unwrap();
        std::fs::create_dir_all(base.join("a/b")).unwrap();
        std::fs::create_dir(base.join("real")).unwrap();
        std::os::unix::fs::symlink(base.join("real"), base.join("link")).unwrap();

        // A symlinked directory, and `..`, are read through.
        assert_eq!(ask(&base.join("link")), Some(s(&base.join("real"))));
        assert_eq!(ask(&base.join("a/../real")), Some(s(&base.join("real"))));
        // Ask through the not-yet-canonical /tmp path too (macOS /tmp is a
        // symlink); the answer is the canonical one either way.
        assert_eq!(ask(&tmp.path().join("a/b")), Some(s(&base.join("a/b"))));
        let requested = base.join("a/b");
        assert_eq!(ask(&requested), Some(s(&requested)));

        // `a` renamed and replaced by a symlink to its new name: the same
        // request now names a2/b.
        std::fs::rename(base.join("a"), base.join("a2")).unwrap();
        std::os::unix::fs::symlink(base.join("a2"), base.join("a")).unwrap();
        assert_eq!(ask(&requested), Some(s(&base.join("a2/b"))));

        // Removed: kept verbatim, not the last answer.
        std::fs::remove_dir(base.join("a2/b")).unwrap();
        assert_eq!(ask(&requested), Some(s(&requested)));

        // Recreated as a real directory again.
        std::fs::remove_file(base.join("a")).unwrap();
        std::fs::create_dir_all(base.join("a/b")).unwrap();
        assert_eq!(ask(&requested), Some(s(&requested)));
    }

    #[test]
    fn chain_of_the_global_entry_is_just_itself() {
        assert_eq!(path_chain(None), vec![None]);
    }

    #[test]
    fn chain_under_home_stops_below_home() {
        let home = HOME;
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
        assert_eq!(path_chain(Some(HOME)), vec![None]);
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
