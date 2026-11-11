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
//! the environment, the directory and the `_.exec()` capability are.
//!
//! A miss and a failure are different answers: `Ok(None)` is "nothing is
//! stored at that address", `Err` is "something is wrong".
//!
//! See `docs/decisionlog/261007074623-field-value-by-coalescing-facets.md`.

use crate::engine::{EngineCtx, engine_env, with_ctx};
use crate::eval::{
    EvalError, Import, MAX_VIRTUAL_DEPTH, Probe, discover_refs, evaluate, imports, library,
    too_deep,
};
use crate::lazy::{Bridge, LazyNode, Prefetch};
use crate::library::memo::{Cached, Frame, Leaf, Memo, MemoFrames};
use crate::protocol::Wait;
use crate::virtual_fields::{Ref, mj_to_json};
use minijinja::Environment;
use minijinja::value::{Serde, Value as MjValue};
use serde_json::{Map, Value as JsonValue};
use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, PoisonError, RwLock, mpsc};

/// The node of a provider's `:template` tree its `var.<name>` bindings
/// stand at, `<p>:template.var.<name>`; their memos are `<p>:memo.var.<name>`.
/// Not a field: the daemon takes it out of the tree a read walks and sends it
/// beside the template half instead ([`Got::var`]).
pub const VAR_NODE: &str = "var";

/// One provider's var table as the daemon sent it: each binding's source by
/// name, and the memo half beside them.
#[derive(Clone)]
struct VarTable {
    sources: Map<String, JsonValue>,
    memo: Option<JsonValue>,
}

/// One node as the daemon returned it: the `get` response of design §3.4.
///
/// `data` is the cached value at the address, `template` the template tree in
/// effect there — a string for a leaf, an object for a group. `rest` is the
/// address left over when the template walk stopped at a string, so the
/// client can walk it into what that string renders to. `memo` is what
/// `| cache()` stored for the template's leaves, beside the template and
/// shaped like it ([`crate::protocol::Response::memo`]). `age_ms` accompanies
/// `data` and nothing else. All-`None` is a miss.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Got {
    /// The cached value at the address, if any.
    pub data: Option<JsonValue>,
    /// The template in effect at the address: a string leaf or an object tree.
    pub template: Option<JsonValue>,
    /// The address left over past a template string, to walk into what it renders.
    pub rest: Option<String>,
    /// The memo half: one memo object for a string template, a tree of them
    /// for a group, present only with `template`.
    pub memo: Option<JsonValue>,
    /// Age of `data`, present only with it.
    pub age_ms: Option<u128>,
    /// The log lines behind the node ([`crate::protocol::Response::log`]),
    /// sent only when the request asked for them.
    pub log: Vec<crate::protocol::LogLine>,
    /// The provider's vars, by name, sent beside a template half when it has
    /// any ([`crate::protocol::Response::var`]).
    pub var: Option<JsonValue>,
    /// The vars' memos, by name.
    pub var_memo: Option<JsonValue>,
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
    /// The caller's environment, the only source of `env.*`: a map, or
    /// [`crate::env::Lazy`] to read the process's only if a render needs it.
    pub env: &'a (dyn crate::env::Vars + Sync),
    /// Which directory this resolution addresses.
    pub slot: Slot,
    /// Re-execute the source before answering.
    pub force: bool,
    /// How long each fetch waits for a run it needs ([`Wait`]).
    pub wait: Wait,
    /// Whether an evaluated template may call `_.exec()`.
    pub allow_exec: bool,
    /// The clock `_.now()`, `ago` and `spinner` read: one value for the whole
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
            Slot::Global => self.env.vars().get("HOME").map(String::as_str),
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
/// inside a template naming a rejected key is a missing ref, and a
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

/// What the daemon is asked by a resolution.
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
        wait: Wait,
    ) -> Result<Option<Got>, FetchError>;

    /// Store the memo a cached leaf rendered ([`MemoWrite`]): the `memo` op.
    fn memo(&mut self, write: &MemoWrite) -> Result<(), FetchError>;
}

/// What a template leaf whose `cache()` missed rendered, to be stored as its
/// memo: the `memo` op's request.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoWrite {
    /// The leaf's address, `provider.field`.
    pub key: String,
    /// The directory the resolution read from; `None` is the global entry.
    pub path: Option<String>,
    /// The hash `cache()` computed over the leaf's inputs and files.
    pub hash: u64,
    /// The leaf's final output; `None` for a leaf that rendered nothing (a
    /// memo of absence).
    pub data: Option<JsonValue>,
    /// The files `watch()` recorded, absolute.
    pub watch: Vec<std::path::PathBuf>,
    /// The lifetime `ttl()` recorded, whole seconds.
    pub ttl: Option<std::time::Duration>,
    /// The lines the leaf's render raised, which a hit replays.
    pub log: Vec<crate::protocol::LogLine>,
}

/// Store each memo a resolution's cached leaves rendered, after its value has
/// gone to whoever asked: the one flush, every surface's. A write that fails
/// is dropped: the value was already rendered, the memo stays cold, and the
/// next render of the leaf tries again.
pub fn write_back(memos: &[MemoWrite], fetch: &mut dyn Fetch) {
    for write in memos {
        let _ = fetch.memo(write);
    }
}

/// Why a resolution failed.
#[derive(Debug, PartialEq, Eq)]
pub enum ResolveError {
    /// A template that does not compile, a cycle, or the depth limit — the
    /// message names the field, and someone has to go and edit it.
    Config(String),
    /// A failed or disabled `_.exec()`, or any other render-time failure.
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

/// The macro libraries a resolution has read, each text by the name an
/// import writes it with: what its engine's loader hands out.
type Libraries = Arc<RwLock<BTreeMap<String, String>>>;

/// The value `comb get <key>` prints. `Ok(None)` is a miss. The memos its
/// cached leaves rendered are written back before it returns ([`write_back`]):
/// a library call has no "after the caller has it".
pub fn resolve(
    key: &str,
    inputs: &Inputs<'_>,
    fetch: &mut dyn Fetch,
) -> Result<Option<JsonValue>, ResolveError> {
    let traced = resolve_traced(key, inputs, fetch)?;
    write_back(&traced.memos, fetch);
    Ok(traced.value)
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
    /// `_.exec()` or literals alone is computed now. A rendered value has no
    /// age of its own; it is as old as the oldest input behind it, and a
    /// value served from a memo is at least as old as the memo.
    pub oldest_input_ms: Option<u128>,
    /// The memos its cached leaves rendered, not yet written: the caller
    /// hands its value on first, then calls [`write_back`].
    pub memos: Vec<MemoWrite>,
    /// Whether the addressed leaf was served from its memo.
    pub memo_hit: bool,
    /// The log lines behind the value ([`resolve_logged`]): the daemon's for
    /// each node it fetched, then the ones its renders raised here. Empty
    /// unless asked for.
    pub log: Vec<crate::protocol::LogLine>,
}

/// [`resolve`], traced, with its memos left for the caller to write back
/// once the value is out ([`write_back`]).
pub fn resolve_traced(
    key: &str,
    inputs: &Inputs<'_>,
    fetch: &mut dyn Fetch,
) -> Result<Traced, ResolveError> {
    resolve_logged(key, inputs, fetch, None)
}

/// [`resolve_traced`], with the log lines at `level` and above that its
/// renders raise here in [`Traced::log`], after the ones the daemon sent
/// with each node (which a [`Fetch`] asks for on its own: a `Session`'s
/// [`set_log`](crate::Session::set_log)). `None` collects nothing.
pub fn resolve_logged(
    key: &str,
    inputs: &Inputs<'_>,
    fetch: &mut dyn Fetch,
    level: Option<crate::protocol::Level>,
) -> Result<Traced, ResolveError> {
    let mut r = Resolver::new(inputs, fetch);
    let (value, rendered) = match level {
        Some(level) => crate::library::log::capture(level, "template", true, || r.resolve(key)),
        None => (r.resolve(key), Vec::new()),
    };
    let value = value?;
    crate::library::log::merge(&mut r.log, rendered);
    Ok(Traced {
        value,
        deps: r.order,
        oldest_input_ms: r.oldest,
        memos: r.memos,
        memo_hit: r.memo_hit,
        log: r.log,
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
    let traced = resolve_traced(key, inputs, fetch)?;
    write_back(&traced.memos, fetch);
    Ok(traced.deps)
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
/// name — `env`, `comb`, a provider ref, `_.exec()` — reads as it does in any
/// template.
///
/// The source is stored nowhere, so its own `cache()`, `watch()` and `ttl()`
/// pass their input through and record nothing; a field it reads whose
/// template caches is served and stored as every read of that field is, and
/// its memo is written back before this returns.
pub fn eval_with(
    src: &str,
    name: &str,
    inputs: &Inputs<'_>,
    fetch: &mut dyn Fetch,
    locals: &Map<String, JsonValue>,
) -> Result<Option<JsonValue>, ResolveError> {
    let mut r = Resolver::new(inputs, fetch);
    r.push(name)?;
    let rendered = r.render_frame_with(src, name, None, locals, Frame::Format);
    r.stack.pop();
    let memos = std::mem::take(&mut r.memos);
    write_back(&memos, r.fetch);
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
    /// The one engine every render of this resolution runs in, and every
    /// text's refs are read in: the client's engine with its loader the
    /// [`Libraries`] read so far. Shared, so a render on another thread (a
    /// lazy `comb`) runs in it too. Made on the first render, so a read that
    /// renders nothing never pays for it ([`Self::jinja`]).
    jinja: OnceCell<Arc<Probe<'static>>>,
    /// What every render of this resolution runs with ([`with_ctx`]): the
    /// caller's `env` and directory, the only ones an `_.exec()` child sees,
    /// and the memo frames. Made on the first render too ([`Self::ctx`]).
    ctx: OnceCell<Arc<EngineCtx>>,
    /// The macro libraries read so far ([`Self::closure`]).
    libraries: Libraries,
    /// The caller's environment as a template reads it, `env`: built once,
    /// and shared by every render rather than copied into each — an engine
    /// value is reference-counted, so binding it is a pointer copy. Built on
    /// the first render too ([`Self::env`]).
    env: OnceCell<MjValue>,
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
    /// The frames of the renders in progress, which the memo filters read
    /// ([`crate::library::memo`]): the engine's [`EngineCtx::memo`].
    frames: Arc<MemoFrames>,
    /// The memos the leaves rendered so far missed, to write back.
    memos: Vec<MemoWrite>,
    /// Whether the last leaf to finish rendering was served from its memo.
    last_hit: bool,
    /// Whether the addressed leaf was ([`Traced::memo_hit`]).
    memo_hit: bool,
    /// Each provider's var table, as the daemon sent it beside a template
    /// half; a provider with none has no entry.
    var_tables: HashMap<String, VarTable>,
    /// What each var resolved to so far, by its `(p.var.<name>, path)`: as
    /// [`Self::resolved`] is for a node.
    vars: HashMap<Coord, Result<Option<JsonValue>, String>>,
    /// The log lines the daemon sent with the nodes fetched so far.
    log: Vec<crate::protocol::LogLine>,
}

impl<'a> Resolver<'a> {
    fn new(inputs: &'a Inputs<'a>, fetch: &'a mut dyn Fetch) -> Self {
        Self {
            inputs,
            jinja: OnceCell::new(),
            ctx: OnceCell::new(),
            libraries: Libraries::default(),
            env: OnceCell::new(),
            fetch,
            memo: HashMap::new(),
            resolved: HashMap::new(),
            order: Vec::new(),
            stack: Vec::new(),
            oldest: None,
            frames: Arc::default(),
            memos: Vec::new(),
            last_hit: false,
            memo_hit: false,
            var_tables: HashMap::new(),
            vars: HashMap::new(),
            log: Vec::new(),
        }
    }

    /// The render context, made on first use: a plain cached value never
    /// renders, and copying the caller's whole environment into it is most
    /// of a read's client-side cost.
    fn ctx(&self) -> &Arc<EngineCtx> {
        self.ctx.get_or_init(|| {
            Arc::new(EngineCtx {
                env: self.inputs.env.vars().clone(),
                cwd: self.inputs.dir().map(Into::into),
                allow_exec: self.inputs.allow_exec,
                now: self.inputs.now,
                // A client's render: nothing checks what it read.
                touched: None,
                memo: Some(self.frames.clone()),
                // Set per leaf, as each renders ([`Self::render_as`]).
                ..Default::default()
            })
        })
    }

    /// The engine, made on first use.
    fn jinja(&self) -> &Arc<Probe<'static>> {
        self.jinja.get_or_init(|| {
            let mut jinja = engine_env();
            // An import loads a library this resolution has read. A name it
            // has not is the engine's "template not found": only a computed
            // name could reach that, and the structural scan refuses those
            // first.
            let loaded = self.libraries.clone();
            jinja.set_loader(move |name| {
                let loaded = loaded.read().unwrap_or_else(PoisonError::into_inner);
                Ok(loaded.get(name).cloned())
            });
            Arc::new(Probe::new(jinja))
        })
    }

    /// The caller's environment as `env`, built on first use. In name order,
    /// so `env | items` and a loop over `env` read the same order every run.
    fn env(&self) -> &MjValue {
        self.env.get_or_init(|| {
            MjValue::from(Serde(
                self.inputs.env.vars().iter().collect::<BTreeMap<_, _>>(),
            ))
        })
    }

    /// The data of `got`, noted as an input this resolution used.
    fn used(&mut self, got: Option<Got>) -> Option<JsonValue> {
        let got = got?;
        if got.data.is_some()
            && let Some(age) = got.age_ms
        {
            self.aged(age);
        }
        got.data
    }

    /// An input `age` old was used.
    fn aged(&mut self, age: u128) {
        self.oldest = Some(self.oldest.map_or(age, |o| o.max(age)));
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
        // The addressed node, rather than one a render of it reads.
        let addressed = self.stack.is_empty();
        match got.template {
            // A leaf template. An address that ran past it is walked into
            // what it rendered — an object by key, an array by index. Its
            // memo is the leaf's, at the address the template stands at.
            Some(JsonValue::String(t)) => {
                let leaf = leaf_key(key, got.rest.as_deref());
                let value = self.render(&t, key, &leaf, got.memo.as_ref())?;
                if addressed {
                    self.memo_hit = self.last_hit;
                }
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
                let rendered = self.render_tree(&tree, key, got.memo.as_ref())?;
                let data = self.used(Some(Got {
                    template: None,
                    ..got
                }));
                let stored_empty = matches!(&data, Some(JsonValue::Object(d)) if d.is_empty());
                let merged = match data {
                    Some(JsonValue::Object(mut data)) => {
                        prune_template_owned(&mut data, &tree);
                        crate::merge::deep_merge(&mut data, rendered);
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
                // Its lines are the resolution's, once: a node fetched again
                // is answered from the memo, without them.
                Ok(Some(mut got)) if !got.log.is_empty() => {
                    crate::library::log::merge(&mut self.log, std::mem::take(&mut got.log));
                    Ok((got != Got::default()).then_some(got))
                }
                Ok(got) => Ok(got),
                Err(FetchError::Rejected(m)) => Err(m),
                Err(e @ FetchError::Transport(_)) => return Err(ResolveError::Fetch(e)),
            };
        if let Ok(Some(got)) = &answer {
            self.note_vars(key, got);
        }
        self.memo.insert(coord.clone(), answer.clone());
        self.order.push(coord);
        answer.map_err(rejected)
    }

    /// [`Self::fetch`] for a ref inside a template: a key the daemon rejected
    /// is a missing ref, and a missing ref is falsy at any depth, so
    /// it binds nothing. A transport failure still stops the resolution.
    fn fetch_ref(&mut self, key: &str) -> Result<Option<Got>, ResolveError> {
        match self.fetch(key) {
            // Likely a typo (a provider that is not one): said, where asked.
            Err(ResolveError::Fetch(FetchError::Rejected(m))) => {
                if crate::library::log::wanted(crate::protocol::Level::Warn) {
                    let msg = format!("reads {key}, which is refused, so absent: {m}");
                    crate::library::log::raise(crate::protocol::Level::Warn, &msg);
                }
                Ok(None)
            }
            other => other,
        }
    }

    /// Render one template leaf's source, as the node addressed by `key`:
    /// the leaf at `leaf`, whose memo half the daemon sent as `memo`.
    /// `Ok(None)` is absent: a one-tag source whose value is `none` or
    /// undefined.
    fn render(
        &mut self,
        src: &str,
        key: &str,
        leaf: &str,
        memo: Option<&JsonValue>,
    ) -> Result<Option<JsonValue>, ResolveError> {
        self.push(key)?;
        let frame = Frame::Leaf(Leaf::new(
            leaf.to_string(),
            memo.and_then(Memo::from_half),
            self.inputs.force,
            self.inputs.wait,
        ));
        // The provider whose leaf (or var) this is: its vars are in scope.
        let provider = crate::key::split_key(key)
            .ok()
            .and_then(|(head, _)| crate::key::split_facet(head).ok())
            .and_then(|(base, facet)| facet.is_none().then(|| base.to_string()));
        let rendered = self.render_frame_with(src, key, provider.as_deref(), &Map::new(), frame);
        self.stack.pop();
        rendered
    }

    /// Render `src` as `key`, with `locals` bound over the refs (a ref whose
    /// name a local binds is not fetched), in `frame`: a leaf's, or a
    /// format's.
    ///
    /// The refs are those of `src` and of every library in its closure: a
    /// library renders in its importer's context, so what it reads is bound
    /// there, used or not, and a `comb[...]` anywhere in the closure makes
    /// `comb` the lazy node.
    fn render_frame_with(
        &mut self,
        src: &str,
        key: &str,
        provider: Option<&str>,
        locals: &Map<String, JsonValue>,
        frame: Frame,
    ) -> Result<Option<JsonValue>, ResolveError> {
        let libraries = self.closure(src, key)?;
        let mut refs: Vec<Ref> = std::iter::once(src)
            .chain(libraries.iter().map(String::as_str))
            .flat_map(|text| discover_refs(self.jinja(), text))
            .filter(|r| match r {
                Ref::Dynamic(name) | Ref::Resolved(name, _) => !locals.contains_key(name),
                Ref::Env(_) | Ref::CombField(..) | Ref::CombProvider(_) => true,
            })
            .collect();
        refs.sort();
        refs.dedup();
        let vars = match provider {
            Some(provider) => self.bind_vars(provider, &mut refs)?,
            None => Map::new(),
        };
        let mut bound = self.bind(&refs)?;
        bound.extend(vars);
        for (name, value) in locals {
            bound.insert(name.clone(), value.clone());
        }
        // The frame goes on once everything the render reads is bound, so
        // the renders that binding made have come and gone: whatever runs
        // while it is on top is this render.
        let leaf = matches!(frame, Frame::Leaf(_));
        self.frames.push(frame);
        // `comb[...]` reads what the render asks for, so `comb` is a lazy
        // node rather than a prefetched value.
        let render = || {
            if refs.contains(&Ref::Dynamic("comb".to_string())) {
                self.render_with_lazy_comb(src, key, bound, &refs)
            } else {
                let context = self.context(None, bound);
                self.as_leaf(key, || {
                    with_ctx(self.ctx().clone(), || {
                        evaluate_node(self.jinja().env(), src, key, context)
                    })
                })
            }
        };
        // A leaf's lines are kept for its memo, `info` and above (`debug` too
        // where the read asks for it), and are the read's as well.
        // What the render read and how long it took, where a read asks.
        let started = crate::library::log::wanted(crate::protocol::Level::Debug)
            .then(std::time::Instant::now);
        let (rendered, lines) = if leaf {
            let min = if crate::library::log::wanted(crate::protocol::Level::Debug) {
                crate::protocol::Level::Debug
            } else {
                crate::protocol::Level::Info
            };
            crate::library::log::capture(min, "template", true, render)
        } else {
            (render(), Vec::new())
        };
        crate::library::log::extend(lines.clone());
        if let Some(started) = started {
            let read: Vec<String> = refs
                .iter()
                .map(|r| match r {
                    Ref::Env(name) => format!("env.{name}"),
                    Ref::CombField(p, f) => format!("comb.{p}.{f}"),
                    Ref::CombProvider(p) => format!("comb.{p}"),
                    Ref::Resolved(p, rest) => crate::key::join(p, rest),
                    Ref::Dynamic(name) => name.clone(),
                })
                .collect();
            let read = if read.is_empty() {
                "nothing".to_string()
            } else {
                read.join(", ")
            };
            let took = format!("{:.1?}", started.elapsed());
            crate::library::log::raise_at(
                crate::protocol::Level::Debug,
                None,
                Some(key),
                &format!("rendered in {took}, reading {read}"),
            );
        }
        if let Some(Frame::Leaf(leaf)) = self.frames.pop() {
            self.settle(leaf, &rendered, lines);
        }
        rendered
    }

    /// What a leaf's `cache()` decided, once the leaf has rendered: a hit is
    /// as old as its memo; a miss that rendered is queued to be written back
    /// with what it recorded, and one that failed writes nothing.
    fn settle(
        &mut self,
        leaf: Leaf,
        rendered: &Result<Option<JsonValue>, ResolveError>,
        lines: Vec<crate::protocol::LogLine>,
    ) {
        self.last_hit = leaf.cached == Some(Cached::Hit);
        match (leaf.cached, rendered) {
            (Some(Cached::Hit), _) => {
                if let Some(memo) = leaf.memo {
                    self.aged(memo.age_ms);
                    // The lines the render that stored the memo raised are
                    // the value's: replayed, from the memo.
                    let replayed = memo
                        .log
                        .into_iter()
                        .map(|mut line| {
                            line.origin = "memo".into();
                            line.key = Some(leaf.key.clone());
                            line
                        })
                        .collect();
                    crate::library::log::extend(replayed);
                }
            }
            (Some(Cached::Miss(hash)), Ok(value)) => self.memos.push(MemoWrite {
                key: leaf.key,
                path: self.inputs.path().map(str::to_string),
                hash,
                data: value.clone(),
                watch: leaf.paths,
                ttl: leaf.ttl,
                // comb's reasons were this read's, and are not the value's.
                log: lines
                    .into_iter()
                    .filter(|l| l.origin != crate::library::memo::ORIGIN)
                    .collect(),
            }),
            _ => {}
        }
    }

    /// Read every macro library `src` imports by name, and every one those
    /// import, into this resolution's [`Libraries`]: each address fetched
    /// once, through the memo, so it is a dependency a `watch` follows. A
    /// library is text, like a template, so its age is no input's. The texts
    /// of the closure, nearest import first, each once.
    ///
    /// An import whose name is not a library's address written as a string
    /// is not read: the render refuses it ([`evaluate`]).
    fn closure(&mut self, src: &str, key: &str) -> Result<Vec<String>, ResolveError> {
        let mut texts = Vec::new();
        let mut seen = BTreeSet::new();
        let mut pending: VecDeque<String> = library_imports(src).into();
        while let Some(name) = pending.pop_front() {
            if !seen.insert(name.clone()) {
                continue;
            }
            let text = self.library(&name, key)?;
            pending.extend(library_imports(&text));
            texts.push(text);
        }
        Ok(texts)
    }

    /// The text of the library an import names `name`, read once per
    /// resolution. Nothing there is the leaf's failure,
    /// [`ResolveError::Runtime`], so a group omits it; something other than
    /// a string is configuration someone has to fix.
    fn library(&mut self, name: &str, key: &str) -> Result<String, ResolveError> {
        let read = |libraries: &Libraries| {
            libraries
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .get(name)
                .cloned()
        };
        if let Some(text) = read(&self.libraries) {
            return Ok(text);
        }
        let address = library(name).unwrap_or_else(|| name.to_string());
        let text = match self.fetch_ref(&address)?.and_then(|got| got.data) {
            Some(JsonValue::String(text)) => text,
            None => {
                return Err(ResolveError::Runtime(format!(
                    "{key}: {name}: no macro library there"
                )));
            }
            Some(other) => {
                return Err(ResolveError::Config(format!(
                    "{key}: {name}: a macro library is a string, not {}",
                    json_kind(&other)
                )));
            }
        };
        self.libraries
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.to_string(), text.clone());
        Ok(text)
    }

    /// The context a render evaluates against: the caller's environment as
    /// `env`, a lazy `comb` when the render has one, and every name `bound`
    /// holds. A name `bound` holds wins: a local called `env` is that local.
    fn context(&self, comb: Option<LazyNode>, bound: Map<String, JsonValue>) -> MjValue {
        let env = (!bound.contains_key("env")).then(|| ("env".to_string(), self.env().clone()));
        let comb = comb.map(|node| ("comb".to_string(), MjValue::from_object(node)));
        let bound = bound
            .into_iter()
            .map(|(name, value)| (name, MjValue::from(Serde(&value))));
        MjValue::from_pairs(env.into_iter().chain(comb).chain(bound))
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
        let jinja = self.jinja().clone();
        let ctx = self.ctx().clone();
        let keep = ctx.clone();

        let previous = keep.set_origin(Some(key.to_string()));
        // The lines the render raises on its thread are this one's capture's.
        let capturing = crate::library::log::capturing();
        let out = std::thread::scope(|scope| {
            let render = scope.spawn(move || {
                let render = || with_ctx(ctx, || evaluate_node(jinja.env(), src, key, context));
                let (out, lines) = match capturing {
                    Some((min, origin, keyed)) => {
                        crate::library::log::capture(min, &origin, keyed, render)
                    }
                    None => (render(), Vec::new()),
                };
                (out, bridge.take_error(), lines)
            });
            for (asked, reply) in questions {
                let answer = self.fetch_ref(&asked).map(|g| self.used(g));
                let _ = reply.send(answer);
            }
            let (out, error, lines) = render
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
            crate::library::log::extend(lines);
            match error {
                Some(e) => Err(e),
                None => out,
            }
        });
        keep.set_origin(previous);
        out
    }

    /// Run `f`, a render of the leaf at `key`, with that address as the
    /// origin of the lines it logs.
    fn as_leaf<R>(&self, key: &str, f: impl FnOnce() -> R) -> R {
        let ctx = self.ctx();
        let previous = ctx.set_origin(Some(key.to_string()));
        let out = f();
        ctx.set_origin(previous);
        out
    }

    /// Render every string leaf of a template tree, keeping its shape.
    ///
    /// The group is on the stack around its leaves, so a leaf that names its
    /// own group is a cycle rather than an infinite assembly. A leaf naming a
    /// sibling resolves that sibling alone — refs carry their full path — so
    /// nothing here needs to be skipped.
    ///
    /// `memo` is the group's memo half, a tree shaped like `tree`: each leaf
    /// renders with the memo at its own place in it.
    fn render_tree(
        &mut self,
        tree: &Map<String, JsonValue>,
        key: &str,
        memo: Option<&JsonValue>,
    ) -> Result<Map<String, JsonValue>, ResolveError> {
        self.push(key)?;
        let rendered = self.render_tree_frame(tree, key, memo);
        self.stack.pop();
        rendered
    }

    fn render_tree_frame(
        &mut self,
        tree: &Map<String, JsonValue>,
        key: &str,
        memo: Option<&JsonValue>,
    ) -> Result<Map<String, JsonValue>, ResolveError> {
        let mut out = Map::new();
        // `serde_json::Map` is a `BTreeMap` here, so leaves render in name
        // order — one deterministic order for the errors and the fetches.
        for (name, node) in tree {
            let leaf = crate::key::push(key, name);
            let memo = memo.and_then(|m| m.get(name));
            match node {
                JsonValue::String(src) => {
                    match self.render(src, &leaf, &leaf, memo) {
                        Ok(Some(value)) => {
                            out.insert(name.clone(), value);
                        }
                        // An absent leaf, or one that failed at render time,
                        // is omitted; one that does not compile, cycles or
                        // could not be fetched fails the whole query, already
                        // named.
                        Ok(None) => {}
                        // Left out of the value, so said where a read asks.
                        Err(ResolveError::Runtime(m)) => {
                            crate::library::log::raise_at(
                                crate::protocol::Level::Error,
                                None,
                                Some(&leaf),
                                &m,
                            );
                        }
                        Err(e) => return Err(e),
                    }
                }
                // A subgroup with nothing present in it is absent, so it is
                // not a member either.
                JsonValue::Object(sub) => {
                    let value = self.render_tree(sub, &leaf, memo)?;
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

    /// The vars of `provider` that `refs` name, each computed (at most once
    /// per resolution) and taken out of `refs`, so a var hides a provider of
    /// its name. An absent var binds nothing, as a missing ref does. A name
    /// alone is a ref, so a render with none pays no fetch for the table.
    fn bind_vars(
        &mut self,
        provider: &str,
        refs: &mut Vec<Ref>,
    ) -> Result<Map<String, JsonValue>, ResolveError> {
        let mut bound = Map::new();
        let named = |r: &Ref| match r {
            Ref::Dynamic(name) | Ref::Resolved(name, _) => Some(name.clone()),
            _ => None,
        };
        let names: BTreeSet<String> = refs
            .iter()
            .filter_map(named)
            .filter(|n| n != "env" && n != "comb")
            .collect();
        if names.is_empty() {
            return Ok(bound);
        }
        // The table came beside the template half of the leaf being
        // rendered: a provider with no vars sent none, and costs nothing.
        let Some(table) = self.var_tables.get(provider).cloned() else {
            return Ok(bound);
        };
        let names: BTreeSet<String> = names
            .into_iter()
            .filter(|n| table.sources.contains_key(n))
            .collect();
        for name in &names {
            if let Some(value) = self.var(provider, name, &table)? {
                bound.insert(name.clone(), value);
            }
        }
        refs.retain(|r| named(r).is_none_or(|n| !names.contains(&n)));
        Ok(bound)
    }

    /// Keep the var table the daemon sent beside a template half of `key`'s
    /// provider ([`Got::var`]): the first one this resolution was sent.
    fn note_vars(&mut self, key: &str, got: &Got) {
        let Some(JsonValue::Object(sources)) = &got.var else {
            return;
        };
        let Ok((head, _)) = crate::key::split_key(key) else {
            return;
        };
        if let Ok((provider, None)) = crate::key::split_facet(head) {
            self.var_tables
                .entry(provider.to_string())
                .or_insert_with(|| VarTable {
                    sources: sources.clone(),
                    memo: got.var_memo.clone(),
                });
        }
    }

    /// The value of `provider`'s var `name`, rendered once per resolution as
    /// a leaf at `<p>.var.<name>`, so its `cache()` memoises it there. A
    /// render failure is the reading leaf's; a var reading itself, however
    /// far round, is a cycle.
    fn var(
        &mut self,
        provider: &str,
        name: &str,
        table: &VarTable,
    ) -> Result<Option<JsonValue>, ResolveError> {
        let key = crate::key::join(provider, &crate::key::push(VAR_NODE, name));
        let coord = self.coord(&key);
        if !self.on_stack(&key)
            && let Some(done) = self.vars.get(&coord)
        {
            return done.clone().map_err(ResolveError::Runtime);
        }
        let Some(JsonValue::String(src)) = table.sources.get(name) else {
            return Err(ResolveError::Config(format!(
                "{key}: a var is a string (a value expression)"
            )));
        };
        let memo = table.memo.as_ref().and_then(|m| m.get(name));
        let outcome = self.render(src, &key, &key, memo);
        match &outcome {
            Ok(value) => {
                self.vars.insert(coord, Ok(value.clone()));
            }
            Err(ResolveError::Runtime(m)) => {
                self.vars.insert(coord, Err(m.clone()));
            }
            Err(_) => {}
        }
        outcome
    }

    /// The caller's `$var`; `None` when unset.
    fn env_value(&self, var: &str) -> Option<String> {
        self.inputs.env.vars().get(var).cloned()
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

/// The libraries `src` imports by name, in source order: each import whose
/// name is a library's address written as a string.
fn library_imports(src: &str) -> Vec<String> {
    imports(src)
        .into_iter()
        .filter_map(|import| match import {
            Import::Named(name) if library(&name).is_some() => Some(name),
            Import::Named(_) | Import::Computed(_) => None,
        })
        .collect()
}

/// The address of the template leaf a read of `key` rendered: `key`, less the
/// `rest` the daemon could not walk past the leaf's string.
fn leaf_key(key: &str, rest: Option<&str>) -> String {
    let Some(rest) = rest else {
        return key.to_string();
    };
    let Ok((head, address)) = crate::key::split_key(key) else {
        return key.to_string();
    };
    let segments = crate::key::segments(&address);
    let kept = segments
        .len()
        .saturating_sub(crate::key::segments(rest).len());
    crate::key::join(head, &crate::key::format_address(&segments[..kept]))
}

/// What a value is, for a message that must not carry the value.
fn json_kind(value: &JsonValue) -> &'static str {
    match value {
        JsonValue::Null => "none",
        JsonValue::Bool(_) => "a bool",
        JsonValue::Number(_) => "a number",
        JsonValue::String(_) => "a string",
        JsonValue::Array(_) => "a list",
        JsonValue::Object(_) => "a map",
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
pub fn walk(value: JsonValue, rest: &str) -> Option<JsonValue> {
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
