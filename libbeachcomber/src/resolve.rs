//! The one client-side resolver.
//!
//! The daemon holds templates as data (`provider:template`) and never
//! evaluates one: rendering needs the caller's environment and directory, so
//! it is the client's, always. This module is that client half — the single
//! implementation `comb get`, `comb eval`, `bc_get`/`bc_eval` and every
//! SDK answer from, so they cannot disagree about what a key is worth.
//!
//! It deliberately does none of the things a surface does. It opens no
//! socket, reads no environment variable, calls no `current_dir`, and parses
//! no configuration: every input arrives in [`Inputs`] and every daemon
//! answer through a [`Fetch`] the caller supplies. So the whole of it is
//! testable against a map, and a surface decides once — at its edge — what
//! the environment, the directory and the `exec()` capability are.
//!
//! A miss and a failure are different answers: `Ok(None)` is "nothing is
//! stored at that address", `Err` is "something is wrong".
//!
//! See `docs/superpowers/specs/2026-09-05-one-tree-resolution-design.md` §4
//! and canon `field_resolution.md`.

use crate::engine::{EngineCtx, build_engine_env};
use crate::eval::{EvalError, MAX_VIRTUAL_DEPTH, discover_refs, evaluate, too_deep};
use crate::lazy::{Bridge, LazyNode, Prefetch};
use crate::virtual_fields::{Ref, mj_to_json};
use minijinja::Environment;
use minijinja::value::Value as MjValue;
use serde_json::{Map, Value as JsonValue};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, mpsc};

/// One node as the daemon returned it: the `get` response of design §3.4.
///
/// `data` is the cached value at the address, `template` the template tree in
/// effect there — a string for a leaf, an object for a group. `rest` is the
/// address left over when the template walk stopped at a string, so the
/// client can walk it into what that string renders to. `age_ms` and `stale`
/// accompany `data` and nothing else. All-`None` is a miss.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Got {
    /// The cached value at the address, if any.
    pub data: Option<JsonValue>,
    /// The template in effect at the address: a string leaf or an object tree.
    pub template: Option<JsonValue>,
    /// The address left over past a template string, to walk into what it renders.
    pub rest: Option<String>,
    /// Age of `data`, present only with it.
    pub age_ms: Option<u128>,
    /// Staleness of `data`, present only with it.
    pub stale: Option<bool>,
}

/// Which directory a query addresses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Slot {
    /// The caller's own directory, [`Inputs::cwd`].
    Cwd,
    /// An explicit directory.
    Path(String),
    /// The global entry, addressed by sending no path.
    Global,
}

/// Everything a resolution reads. The library reads no ambient state.
pub struct Inputs<'a> {
    /// The caller's directory, addressed by [`Slot::Cwd`].
    pub cwd: &'a str,
    /// The caller's environment, the only source of `env.*`.
    pub env: &'a HashMap<String, String>,
    /// Which directory this resolution addresses.
    pub slot: Slot,
    /// Re-execute the source before answering.
    pub force: bool,
    /// Wait for a value rather than answering with a miss.
    pub wait: bool,
    /// Whether an evaluated template may call `exec()`.
    pub allow_exec: bool,
    /// The clock `now()`, `ago` and `spinner` read: one value for the whole
    /// resolution. Each surface sets it; a test fixes it.
    pub now: std::time::SystemTime,
}

impl Inputs<'_> {
    /// The `path` every fetch of this resolution carries.
    fn path(&self) -> Option<&str> {
        match &self.slot {
            Slot::Cwd => Some(self.cwd),
            Slot::Path(p) => Some(p),
            Slot::Global => None,
        }
    }

    /// The directory a template's `exec`, `lup` and `read` run in: the one
    /// this resolution addresses, whichever surface asked — an explicit path,
    /// else the caller's directory, and for the global entry `$HOME` (the
    /// caller's). `None` when there is none to give.
    fn dir(&self) -> Option<&str> {
        let dir = match &self.slot {
            Slot::Global => self.env.get("HOME").map(String::as_str),
            _ => self.path(),
        };
        dir.filter(|d| !d.is_empty())
    }
}

/// A [`Fetch`] failure.
///
/// Neither is a miss. `Transport` is the daemon not being asked — a broken
/// connection, a timeout — and stops a resolution wherever it happens.
/// `Rejected` is the daemon answering `ok: false` to the key itself: an
/// invalid facet. The key the caller asked for being
/// rejected is an error the caller hears, in the daemon's own words; a ref
/// inside a template naming a rejected key is a missing ref, and canon says a
/// missing ref is falsy at any depth.
#[derive(Debug, PartialEq, Eq)]
pub enum FetchError {
    /// The daemon could not be asked: the transport's failure, as it was.
    Transport(crate::CombError),
    /// The daemon refused the key, with its reason.
    Rejected(String),
}

impl FetchError {
    /// The failure's slug ([`crate::kind`]): a transport failure keeps its
    /// own; a refused key is the daemon's, `server_error`.
    pub fn kind(&self) -> &'static str {
        match self {
            FetchError::Transport(e) => e.kind(),
            FetchError::Rejected(_) => crate::kind::SERVER_ERROR,
        }
    }
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Transport(e) => e.fmt(f),
            FetchError::Rejected(m) => f.write_str(m),
        }
    }
}

/// One request to the daemon.
///
/// `path` is `None` for the global entry. `Ok(None)` is a miss: the daemon
/// answered, and nothing is stored there.
pub trait Fetch {
    /// Fetch one node.
    fn get(
        &mut self,
        key: &str,
        path: Option<&str>,
        force: bool,
        wait: bool,
    ) -> Result<Option<Got>, FetchError>;
}

/// Why a resolution failed.
#[derive(Debug, PartialEq, Eq)]
pub enum ResolveError {
    /// A template that does not compile, a cycle, or the depth limit — the
    /// message names the field, and someone has to go and edit it.
    Config(String),
    /// A failed or disabled `exec()`, or any other render-time failure.
    Runtime(String),
    /// The daemon could not be asked, or refused the key being resolved.
    Fetch(FetchError),
}

impl ResolveError {
    /// The failure's slug ([`crate::kind`]), the one classification every
    /// surface reports. A template that does not compile, cycles or runs past
    /// the depth limit is configuration someone has to go and edit — the
    /// caller's input, `parse_error`. A failure while rendering is
    /// `server_error`, as is the daemon refusing the key; a transport failure
    /// keeps its own.
    pub fn kind(&self) -> &'static str {
        match self {
            ResolveError::Config(_) => crate::kind::PARSE_ERROR,
            ResolveError::Runtime(_) => crate::kind::SERVER_ERROR,
            ResolveError::Fetch(e) => e.kind(),
        }
    }
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::Config(m) | ResolveError::Runtime(m) => f.write_str(m),
            ResolveError::Fetch(e) => e.fmt(f),
        }
    }
}

/// One node: a key at a path, `None` for the global entry.
type Coord = (String, Option<String>);

/// The distinct `(key, path)` pairs one resolution fetched, in first-fetch order.
type Fetched = Vec<Coord>;

/// The `(key, path)` nodes currently being rendered, innermost last.
type Frames = Vec<Coord>;

/// The value `comb get <key>` prints. `Ok(None)` is a miss.
pub fn resolve(
    key: &str,
    inputs: &Inputs<'_>,
    fetch: &mut dyn Fetch,
) -> Result<Option<JsonValue>, ResolveError> {
    Resolver::new(inputs, fetch).resolve(key)
}

/// One resolution's value, and what it read to reach it.
#[derive(Clone, Debug, PartialEq)]
pub struct Traced {
    /// What [`resolve`] returns. `None` is a miss.
    pub value: Option<JsonValue>,
    /// The distinct `(key, path)` pairs it fetched, in first-fetch order.
    pub deps: Fetched,
    /// The age of the oldest cached value it used: a ref's data, or the data
    /// it answered with. `None` when it used none — a template over `env`,
    /// `exec()` or literals alone is computed now. A rendered value has no
    /// age of its own; it is as old as the oldest input behind it.
    pub oldest_input_ms: Option<u128>,
}

/// [`resolve`], traced.
pub fn resolve_traced(
    key: &str,
    inputs: &Inputs<'_>,
    fetch: &mut dyn Fetch,
) -> Result<Traced, ResolveError> {
    let mut r = Resolver::new(inputs, fetch);
    let value = r.resolve(key)?;
    Ok(Traced {
        value,
        deps: r.order,
        oldest_input_ms: r.oldest,
    })
}

/// The distinct daemon keys a resolution of `key` reads — the watch fan-in's
/// subscription set.
///
/// A key that is a miss is included: it is an address the resolution asked
/// about, and a `put` can make it a hit later.
pub fn deps(
    key: &str,
    inputs: &Inputs<'_>,
    fetch: &mut dyn Fetch,
) -> Result<Fetched, ResolveError> {
    resolve_traced(key, inputs, fetch).map(|t| t.deps)
}

/// `comb eval` / `bc_eval`: render a source that is not stored anywhere,
/// every ref resolved as it would be inside a stored template — so
/// `comb eval '{{ p.f }}'` and `comb get p.f` agree by construction.
/// `Ok(None)` is absent, as for [`resolve`].
pub fn eval(
    src: &str,
    inputs: &Inputs<'_>,
    fetch: &mut dyn Fetch,
) -> Result<Option<JsonValue>, ResolveError> {
    eval_with(src, "<eval>", inputs, fetch, &Map::new())
}

/// [`eval`], with `locals` bound as top-level names over the refs, and errors
/// named `name`. A name `locals` binds is never fetched as a provider:
/// `comb get -f '{{ value }}' git.branch` renders the value it read, and
/// `comb status -f '{{ provider }}'` the row's provider, while every other
/// name — `env`, `comb`, a provider ref, `exec()` — reads as it does in any
/// template.
pub fn eval_with(
    src: &str,
    name: &str,
    inputs: &Inputs<'_>,
    fetch: &mut dyn Fetch,
    locals: &Map<String, JsonValue>,
) -> Result<Option<JsonValue>, ResolveError> {
    let mut r = Resolver::new(inputs, fetch);
    r.push(name)?;
    let rendered = r.render_frame_with(src, name, locals);
    r.stack.pop();
    rendered
}

/// One resolution: its inputs, its transport, its two memos and its
/// evaluation stack.
///
/// The memos and the stack are per top-level call. The fetch memo makes
/// `Fetch::get` run exactly once per distinct `(key, path)` — a namespace
/// whose leaves share a ref is one round trip for that ref, not one per leaf,
/// and a key the daemon rejected is asked about once too — and its insertion
/// order is the dependency list [`deps`] returns. The value memo makes each
/// node render once: a leaf several templates read is rendered for the first
/// and handed to the rest. The stack is what makes a cycle an error rather
/// than a hang; its entries are `(key, path)`, because the same key at two
/// directories is two nodes.
struct Resolver<'a> {
    inputs: &'a Inputs<'a>,
    /// The one engine every render of this resolution runs in, built once:
    /// its library bound to the caller's `env` and directory, the only ones
    /// an `exec()` child sees. Shared, so a render on another thread (a lazy
    /// `comb`) runs in it too.
    jinja: Arc<Environment<'static>>,
    /// The caller's environment as a template reads it, `env`: built once,
    /// and shared by every render rather than copied into each — an engine
    /// value is reference-counted, so binding it is a pointer copy.
    env: MjValue,
    fetch: &'a mut dyn Fetch,
    /// The daemon's answers so far: a node, or its refusal of the key.
    memo: HashMap<Coord, Result<Option<Got>, String>>,
    /// What each node resolved to so far: its value, or the message it
    /// failed to render with ([`ResolveError::Runtime`]).
    resolved: HashMap<Coord, Result<Option<JsonValue>, String>>,
    order: Fetched,
    stack: Frames,
    /// The age of the oldest cached value used so far ([`Traced`]).
    oldest: Option<u128>,
}

impl<'a> Resolver<'a> {
    fn new(inputs: &'a Inputs<'a>, fetch: &'a mut dyn Fetch) -> Self {
        Self {
            inputs,
            jinja: Arc::new(build_engine_env(Arc::new(EngineCtx {
                env: inputs.env.clone(),
                cwd: inputs.dir().map(Into::into),
                allow_exec: inputs.allow_exec,
                now: inputs.now,
            }))),
            // In name order, so `env | items` and a loop over `env` read the
            // same order every run.
            env: MjValue::from_serialize(inputs.env.iter().collect::<BTreeMap<_, _>>()),
            fetch,
            memo: HashMap::new(),
            resolved: HashMap::new(),
            order: Vec::new(),
            stack: Vec::new(),
            oldest: None,
        }
    }

    /// The data of `got`, noted as an input this resolution used.
    fn used(&mut self, got: Option<Got>) -> Option<JsonValue> {
        let got = got?;
        if got.data.is_some()
            && let Some(age) = got.age_ms
        {
            self.oldest = Some(self.oldest.map_or(age, |o| o.max(age)));
        }
        got.data
    }

    /// Resolve one address to the value a surface prints.
    fn resolve(&mut self, key: &str) -> Result<Option<JsonValue>, ResolveError> {
        // `env.*` is the caller's shell, never the daemon's. An unset
        // variable is absent, as in the daemon's lines — never an error, and
        // so is anything below a variable, which is a string.
        if let Ok(("env", address)) = crate::key::split_key(key) {
            return Ok(match crate::key::segments(&address).as_slice() {
                [var] => self.env_value(var).map(JsonValue::String),
                _ => None,
            });
        }

        // A node is resolved once, and every later read of it is told the
        // same. Not while it is being rendered: asked for again then, it is a
        // cycle whatever it came to before, and the render below says so.
        let coord = self.coord(key);
        if !self.on_stack(key)
            && let Some(done) = self.resolved.get(&coord)
        {
            return done.clone().map_err(ResolveError::Runtime);
        }
        let outcome = self.resolve_node(key);
        match &outcome {
            Ok(value) => {
                self.resolved.insert(coord, Ok(value.clone()));
            }
            // A render failure is the node's answer too: a group omits the
            // leaf that failed and goes on, and the next reader of the node
            // is told the same rather than rendering it again.
            Err(ResolveError::Runtime(m)) => {
                self.resolved.insert(coord, Err(m.clone()));
            }
            // A refusal is the fetch memo's already, and every other error
            // ends the resolution.
            Err(_) => {}
        }
        outcome
    }

    /// [`Self::resolve`] of a node not yet resolved: what the daemon holds
    /// there, rendered.
    ///
    /// Each input it uses is counted as it is used — fetched into the
    /// dependency list, its age into the oldest — and both are the
    /// resolution's, so a later read the value memo answers has nothing new
    /// to count: its inputs are the ones counted here.
    fn resolve_node(&mut self, key: &str) -> Result<Option<JsonValue>, ResolveError> {
        let Some(got) = self.fetch(key)? else {
            return Ok(None);
        };
        match got.template {
            // A leaf template. An address that ran past it is walked into
            // what it rendered — an object by key, an array by index.
            Some(JsonValue::String(t)) => {
                let value = self.render(&t, key)?;
                Ok(match got.rest {
                    Some(rest) => value.and_then(|v| walk(v, &rest)),
                    None => value,
                })
            }
            // A group. Every string leaf renders, the shape is kept, and the
            // result wins over the cached data it shares an address with.
            // Every address the tree holds a leaf at is the template's,
            // rendered or omitted — cached data never shows through one.
            //
            // A group assembled here is absent when nothing in it is
            // present, as a single absent leaf is. An object the daemon
            // holds as data is a value even when empty: `{}` written by a
            // put or a Source states an empty object.
            Some(JsonValue::Object(tree)) => {
                let rendered = self.render_tree(&tree, key)?;
                let data = self.used(Some(Got {
                    template: None,
                    ..got
                }));
                let stored_empty = matches!(&data, Some(JsonValue::Object(d)) if d.is_empty());
                let merged = match data {
                    Some(JsonValue::Object(mut data)) => {
                        prune_template_owned(&mut data, &tree);
                        deep_merge(&mut data, rendered);
                        data
                    }
                    _ => rendered,
                };
                Ok((stored_empty || !merged.is_empty()).then_some(JsonValue::Object(merged)))
            }
            Some(_) => Err(ResolveError::Config(format!(
                "{key}: template is neither a string nor an object"
            ))),
            None => Ok(self.used(Some(got))),
        }
    }

    /// Ask the daemon for one node, at most once per `(key, path)`.
    ///
    /// A rejection is the daemon's answer about the key and is memoised like a
    /// hit or a miss; a transport failure is not an answer and ends the
    /// resolution, so nothing about it is kept.
    fn fetch(&mut self, key: &str) -> Result<Option<Got>, ResolveError> {
        let coord = self.coord(key);
        let rejected = |m| ResolveError::Fetch(FetchError::Rejected(m));
        if let Some(answer) = self.memo.get(&coord) {
            return answer.clone().map_err(rejected);
        }
        let answer =
            match self
                .fetch
                .get(key, coord.1.as_deref(), self.inputs.force, self.inputs.wait)
            {
                Ok(got) => Ok(got),
                Err(FetchError::Rejected(m)) => Err(m),
                Err(e @ FetchError::Transport(_)) => return Err(ResolveError::Fetch(e)),
            };
        self.memo.insert(coord.clone(), answer.clone());
        self.order.push(coord);
        answer.map_err(rejected)
    }

    /// [`Self::fetch`] for a ref inside a template: a key the daemon rejected
    /// is a missing ref, and a missing ref is falsy at any depth (canon), so
    /// it binds nothing. A transport failure still stops the resolution.
    fn fetch_ref(&mut self, key: &str) -> Result<Option<Got>, ResolveError> {
        match self.fetch(key) {
            Err(ResolveError::Fetch(FetchError::Rejected(_))) => Ok(None),
            other => other,
        }
    }

    /// Render one template source, as the node addressed by `key`. `Ok(None)`
    /// is absent: a one-tag source whose value is `none` or undefined.
    fn render(&mut self, src: &str, key: &str) -> Result<Option<JsonValue>, ResolveError> {
        self.push(key)?;
        let rendered = self.render_frame(src, key);
        self.stack.pop();
        rendered
    }

    fn render_frame(&mut self, src: &str, key: &str) -> Result<Option<JsonValue>, ResolveError> {
        self.render_frame_with(src, key, &Map::new())
    }

    /// [`Self::render_frame`], with `locals` bound over the refs; a ref whose
    /// name a local binds is not fetched.
    fn render_frame_with(
        &mut self,
        src: &str,
        key: &str,
        locals: &Map<String, JsonValue>,
    ) -> Result<Option<JsonValue>, ResolveError> {
        let refs: Vec<Ref> = discover_refs(src)
            .into_iter()
            .filter(|r| match r {
                Ref::Dynamic(name) | Ref::Resolved(name, _) => !locals.contains_key(name),
                Ref::Env(_) | Ref::CombField(..) | Ref::CombProvider(_) => true,
            })
            .collect();
        let mut bound = self.bind(&refs)?;
        for (name, value) in locals {
            bound.insert(name.clone(), value.clone());
        }
        // `comb[...]` reads what the render asks for, so `comb` is a lazy
        // node rather than a prefetched value.
        if refs.contains(&Ref::Dynamic("comb".to_string())) {
            return self.render_with_lazy_comb(src, key, bound, &refs);
        }
        evaluate_node(&self.jinja, src, key, self.context(None, bound))
    }

    /// The context a render evaluates against: the caller's environment as
    /// `env`, a lazy `comb` when the render has one, and every name `bound`
    /// holds. A name `bound` holds wins: a local called `env` is that local.
    fn context(&self, comb: Option<LazyNode>, bound: Map<String, JsonValue>) -> MjValue {
        let env = (!bound.contains_key("env")).then(|| ("env".to_string(), self.env.clone()));
        let comb = comb.map(|node| ("comb".to_string(), MjValue::from_object(node)));
        let bound = bound
            .into_iter()
            .map(|(name, value)| (name, MjValue::from_serialize(&value)));
        env.into_iter().chain(comb).chain(bound).collect()
    }

    /// Render with `comb` a lazy node.
    ///
    /// The render runs on a scoped thread, and this one answers what the node
    /// asks, each through the same fetch an eager ref uses, until the render
    /// is over and the node is gone. An error an answer carried is the
    /// render's, whatever the render made of the nothing the node read
    /// instead.
    fn render_with_lazy_comb(
        &mut self,
        src: &str,
        key: &str,
        mut bound: Map<String, JsonValue>,
        refs: &[Ref],
    ) -> Result<Option<JsonValue>, ResolveError> {
        let (asks, questions) = mpsc::channel();
        let bridge = Arc::new(Bridge::new(asks));
        let prefetched = match bound.remove("comb") {
            Some(JsonValue::Object(map)) => Some(map),
            _ => None,
        };
        let comb = LazyNode::comb(bridge.clone(), prefetched, Arc::new(Prefetch::of(refs)));
        let context = self.context(Some(comb), bound);
        let jinja = self.jinja.clone();

        std::thread::scope(|scope| {
            let render = scope.spawn(move || {
                let out = evaluate_node(&jinja, src, key, context);
                (out, bridge.take_error())
            });
            for (asked, reply) in questions {
                let answer = self.fetch_ref(&asked).map(|g| self.used(g));
                let _ = reply.send(answer);
            }
            let (out, error) = render
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
            match error {
                Some(e) => Err(e),
                None => out,
            }
        })
    }

    /// Render every string leaf of a template tree, keeping its shape.
    ///
    /// The group is on the stack around its leaves, so a leaf that names its
    /// own group is a cycle rather than an infinite assembly. A leaf naming a
    /// sibling resolves that sibling alone — refs carry their full path — so
    /// nothing here needs to be skipped.
    fn render_tree(
        &mut self,
        tree: &Map<String, JsonValue>,
        key: &str,
    ) -> Result<Map<String, JsonValue>, ResolveError> {
        self.push(key)?;
        let rendered = self.render_tree_frame(tree, key);
        self.stack.pop();
        rendered
    }

    fn render_tree_frame(
        &mut self,
        tree: &Map<String, JsonValue>,
        key: &str,
    ) -> Result<Map<String, JsonValue>, ResolveError> {
        let mut out = Map::new();
        // `serde_json::Map` is a `BTreeMap` here, so leaves render in name
        // order — one deterministic order for the errors and the fetches.
        for (name, node) in tree {
            let leaf = crate::key::push(key, name);
            match node {
                JsonValue::String(src) => {
                    match self.render(src, &leaf) {
                        Ok(Some(value)) => {
                            out.insert(name.clone(), value);
                        }
                        // An absent leaf, or one that failed at render time,
                        // is omitted; one that does not compile, cycles or
                        // could not be fetched fails the whole query, already
                        // named.
                        Ok(None) | Err(ResolveError::Runtime(_)) => {}
                        Err(e) => return Err(e),
                    }
                }
                // A subgroup with nothing present in it is absent, so it is
                // not a member either.
                JsonValue::Object(sub) => {
                    let value = self.render_tree(sub, &leaf)?;
                    if !value.is_empty() {
                        out.insert(name.clone(), JsonValue::Object(value));
                    }
                }
                _ => {
                    return Err(ResolveError::Config(format!(
                        "{leaf}: template leaf is not a string"
                    )));
                }
            }
        }
        Ok(out)
    }

    /// Bind every ref a source names into the context it evaluates against.
    ///
    /// `env` is not bound here: every render's context holds the caller's
    /// whole environment ([`Self::context`]), as a Source's `data` line sees
    /// the daemon's — an unset variable is simply not there, so `env.X`,
    /// `env['X']` and `env | items` read it alike. A daemon miss binds
    /// nothing — not the value, and not the object that would have enclosed
    /// it — so `default` and `or` fire on it; a key the daemon rejects is the
    /// same missing ref. Where a whole `comb.P` and a `comb.P.F` disagree, the
    /// whole object wins: it is the authoritative snapshot, whichever ref is
    /// bound first.
    fn bind(&mut self, refs: &[Ref]) -> Result<Map<String, JsonValue>, ResolveError> {
        let mut top = Map::new();
        top.insert("comb".to_string(), JsonValue::Object(Map::new()));

        for r in refs {
            match r {
                Ref::Env(_) => {}
                // `env` is in every render's context, and `comb` is the lazy
                // node a render with `comb[...]` binds.
                Ref::Dynamic(name) if name == "env" || name == "comb" => {}
                // Any other name used other than by attribute — `git[field]`,
                // `scanline | items` — is that whole node, resolved. It wins
                // over the leaves an attribute ref bound below it.
                Ref::Dynamic(name) => match self.resolve(name) {
                    Ok(Some(value)) => {
                        top.insert(name.clone(), value);
                    }
                    Ok(None) | Err(ResolveError::Fetch(FetchError::Rejected(_))) => {}
                    Err(e) => return Err(e),
                },
                Ref::CombField(provider, field) => {
                    let got = self.fetch_ref(&crate::key::join(provider, field))?;
                    let Some(value) = self.used(got) else {
                        continue;
                    };
                    if let Some(JsonValue::Object(cache)) = top.get_mut("comb") {
                        let entry = cache
                            .entry(provider.clone())
                            .or_insert_with(|| JsonValue::Object(Map::new()));
                        if let JsonValue::Object(fields) = entry {
                            fields.entry(field.clone()).or_insert(value);
                        }
                    }
                }
                Ref::CombProvider(provider) => {
                    let got = self.fetch_ref(provider)?;
                    let Some(value) = self.used(got) else {
                        continue;
                    };
                    if let Some(JsonValue::Object(cache)) = top.get_mut("comb") {
                        match (cache.get_mut(provider), value) {
                            (Some(JsonValue::Object(existing)), JsonValue::Object(whole)) => {
                                for (k, v) in whole {
                                    existing.insert(k, v);
                                }
                            }
                            (Some(_), _) => {}
                            (None, value) => {
                                cache.insert(provider.clone(), value);
                            }
                        }
                    }
                }
                // `rest` is the whole remaining address (`widgets.b`), and
                // the value lands at that nested path so the template's own
                // attribute walk (`scanline.widgets.b`) finds it. The key
                // being rejected is, here, a missing ref like any other.
                Ref::Resolved(provider, rest) => {
                    let key = crate::key::join(provider, rest);
                    let value = match self.resolve(&key) {
                        Ok(Some(value)) => value,
                        Ok(None) | Err(ResolveError::Fetch(FetchError::Rejected(_))) => continue,
                        Err(e) => return Err(e),
                    };
                    insert_at(&mut top, &key, value);
                }
            }
        }
        Ok(top)
    }

    /// The caller's `$var`; `None` when unset.
    fn env_value(&self, var: &str) -> Option<String> {
        self.inputs.env.get(var).cloned()
    }

    /// Enter `key`'s frame, refusing a repeat and a chain past the depth limit.
    ///
    /// A cycle names both nodes: the one that asked (the frame on top of the
    /// stack) and the one it asked for, which is already being resolved.
    fn push(&mut self, key: &str) -> Result<(), ResolveError> {
        if self.stack.len() >= MAX_VIRTUAL_DEPTH {
            return Err(ResolveError::Config(format!("{key}: {}", too_deep())));
        }
        if self.on_stack(key) {
            let top = self.stack.last().map(|(k, _)| k.as_str()).unwrap_or(key);
            return Err(ResolveError::Config(format!(
                "{top}: virtual field cycle detected: {key} is already being resolved"
            )));
        }
        self.stack.push(self.coord(key));
        Ok(())
    }

    /// The node `key` names in this resolution: every one of its reads
    /// addresses the same path.
    fn coord(&self, key: &str) -> Coord {
        (key.to_string(), self.inputs.path().map(str::to_string))
    }

    fn on_stack(&self, key: &str) -> bool {
        let path = self.inputs.path();
        self.stack
            .iter()
            .any(|(k, p)| k == key && p.as_deref() == path)
    }
}

/// [`evaluate`] `src` in `jinja` as the node `key` against `context`, its
/// value as JSON. A source that does not compile is someone's to go and fix,
/// [`ResolveError::Config`]; a failure while it runs is the moment's,
/// [`ResolveError::Runtime`]. Either is named by `key`.
fn evaluate_node(
    jinja: &Environment<'_>,
    src: &str,
    key: &str,
    context: MjValue,
) -> Result<Option<JsonValue>, ResolveError> {
    match evaluate(jinja, src, context) {
        Ok(Some(value)) => {
            mj_to_json(value).map_err(|e| ResolveError::Runtime(format!("{key}: {e}")))
        }
        Ok(None) => Ok(None),
        Err(e @ EvalError::Compile { .. }) => Err(ResolveError::Config(format!("{key}: {e}"))),
        Err(e @ EvalError::Runtime { .. }) => Err(ResolveError::Runtime(format!("{key}: {e}"))),
    }
}

/// Walk `rest`, a canonical address, into a rendered value: an object by key,
/// an array by decimal index. A scalar reached with address left over, or a
/// key or index that is not there, is a miss.
fn walk(value: JsonValue, rest: &str) -> Option<JsonValue> {
    // A `rest` is sent only with address left over, so an empty one names
    // nothing.
    if rest.is_empty() {
        return None;
    }
    let mut current = value;
    for segment in crate::key::segments(rest) {
        let segment = segment.as_ref();
        current = match current {
            JsonValue::Object(mut fields) => fields.remove(segment)?,
            JsonValue::Array(mut items) => {
                let index = crate::key::array_index(segment, items.len())?;
                (index < items.len()).then(|| items.swap_remove(index))?
            }
            _ => return None,
        };
    }
    Some(current)
}

/// Drop from `data` every address `tree` holds a string leaf at. Those
/// addresses are the template's — rendered, or omitted when the render
/// failed — and the cached value beneath never shows through one.
fn prune_template_owned(data: &mut Map<String, JsonValue>, tree: &Map<String, JsonValue>) {
    for (name, node) in tree {
        match node {
            JsonValue::String(_) => {
                data.remove(name);
            }
            JsonValue::Object(sub) => {
                if let Some(JsonValue::Object(inner)) = data.get_mut(name) {
                    prune_template_owned(inner, sub);
                }
            }
            _ => {}
        }
    }
}

/// Insert `value` at the key `key` under `map` — its head, then each segment
/// of its address — creating the objects between. A scalar already sitting on
/// the path is left alone — a field of a scalar is not navigable either way.
fn insert_at(map: &mut Map<String, JsonValue>, key: &str, value: JsonValue) {
    let Ok((head, address)) = crate::key::split_key(key) else {
        return;
    };
    let mut path = vec![std::borrow::Cow::Borrowed(head)];
    path.extend(crate::key::segments(&address));
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut cursor = map;
    for segment in parents {
        let entry = cursor
            .entry(segment.to_string())
            .or_insert_with(|| JsonValue::Object(Map::new()));
        let JsonValue::Object(inner) = entry else {
            return;
        };
        cursor = inner;
    }
    cursor.insert(last.to_string(), value);
}

/// Merge `over` into `base`, `over` winning per leaf: the rendered tree wins
/// over the cached data at the same address.
fn deep_merge(base: &mut Map<String, JsonValue>, over: Map<String, JsonValue>) {
    for (key, value) in over {
        match (base.remove(&key), value) {
            (Some(JsonValue::Object(mut b)), JsonValue::Object(o)) => {
                deep_merge(&mut b, o);
                base.insert(key, JsonValue::Object(b));
            }
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}
