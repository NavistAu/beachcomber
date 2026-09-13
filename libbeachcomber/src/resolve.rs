//! The one client-side resolver.
//!
//! The daemon holds templates as data (`provider:template`) and never
//! evaluates one: rendering needs the caller's environment and directory, so
//! it is the client's, always. This module is that client half — the single
//! implementation `comb get`, `comb eval`, `bc_resolve`/`bc_eval` and every
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

use crate::eval::{
    MAX_VIRTUAL_DEPTH, discover_refs_deep, render_template_with, single_tag_expression, too_deep,
};
use crate::virtual_fields::{Ref, build_expression_env, mj_to_json};
use minijinja::value::Value as MjValue;
use serde_json::{Map, Value as JsonValue};
use std::collections::HashMap;

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
}

/// A [`Fetch`] failure — a transport fault, not a miss.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchError(pub String);

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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolveError {
    /// A template that does not compile, a cycle, or the depth limit — the
    /// message names the field, and someone has to go and edit it.
    Config(String),
    /// A failed or disabled `exec()`, or any other render-time failure.
    Runtime(String),
    /// The daemon could not be asked.
    Fetch(FetchError),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::Config(m) | ResolveError::Runtime(m) => f.write_str(m),
            ResolveError::Fetch(FetchError(m)) => f.write_str(m),
        }
    }
}

/// The distinct `(key, path)` pairs one resolution fetched, in first-fetch order.
type Fetched = Vec<(String, Option<String>)>;

/// The value `comb get <key>` prints. `Ok(None)` is a miss.
pub fn resolve(
    key: &str,
    inputs: &Inputs<'_>,
    fetch: &mut dyn Fetch,
) -> Result<Option<JsonValue>, ResolveError> {
    Resolver::new(inputs, fetch).resolve(key)
}

/// [`resolve`], plus the distinct `(key, path)` pairs it fetched, in
/// first-fetch order.
pub fn resolve_with_deps(
    key: &str,
    inputs: &Inputs<'_>,
    fetch: &mut dyn Fetch,
) -> Result<(Option<JsonValue>, Fetched), ResolveError> {
    let mut r = Resolver::new(inputs, fetch);
    let value = r.resolve(key)?;
    Ok((value, r.order))
}

/// The distinct daemon keys a resolution of `key` reads — the watch fan-in's
/// subscription set.
pub fn deps(
    key: &str,
    inputs: &Inputs<'_>,
    fetch: &mut dyn Fetch,
) -> Result<Fetched, ResolveError> {
    resolve_with_deps(key, inputs, fetch).map(|(_, d)| d)
}

/// `comb eval` / `bc_eval`: render a source that is not stored anywhere,
/// every ref resolved as it would be inside a stored template — so
/// `comb eval '{{ p.f }}'` and `comb get p.f` agree by construction.
pub fn eval(
    src: &str,
    inputs: &Inputs<'_>,
    fetch: &mut dyn Fetch,
) -> Result<JsonValue, ResolveError> {
    Resolver::new(inputs, fetch).render(src, "<eval>")
}

/// The metadata suffixes, whose answers are about a node rather than of it:
/// opaque, and never rendered.
const METADATA_SUFFIXES: [&str; 5] = [":age", ":stale", ":fresh", ":cache", ":source"];

/// One resolution: its inputs, its transport, its fetch memo and its
/// evaluation stack.
///
/// Both the memo and the stack are per top-level call. The memo makes
/// `Fetch::get` run exactly once per distinct `(key, path)` — a namespace
/// whose leaves share a ref is one round trip for that ref, not one per leaf
/// — and its insertion order is the dependency list [`deps`] returns. The
/// stack is what makes a cycle an error rather than a hang; its entries are
/// `(key, path)`, because the same key at two directories is two nodes.
struct Resolver<'a> {
    inputs: &'a Inputs<'a>,
    fetch: &'a mut dyn Fetch,
    memo: HashMap<(String, Option<String>), Option<Got>>,
    order: Fetched,
    stack: Fetched,
}

impl<'a> Resolver<'a> {
    fn new(inputs: &'a Inputs<'a>, fetch: &'a mut dyn Fetch) -> Self {
        Self {
            inputs,
            fetch,
            memo: HashMap::new(),
            order: Vec::new(),
            stack: Vec::new(),
        }
    }

    /// Resolve one address to the value a surface prints.
    fn resolve(&mut self, key: &str) -> Result<Option<JsonValue>, ResolveError> {
        // A metadata suffix asks about a node, not for it. Whatever comes
        // back is the answer — a template-shaped string stored as a source
        // label is a label, not a template.
        if METADATA_SUFFIXES.iter().any(|s| key.ends_with(s)) {
            return Ok(self.fetch(key)?.and_then(|g| g.data));
        }
        // Canon invariant 6: `env.*` is the caller's shell, never the daemon's.
        if key == "env" {
            return Ok(None);
        }
        if let Some(var) = key.strip_prefix("env.") {
            return Ok(self.inputs.env.get(var).cloned().map(JsonValue::String));
        }

        let Some(got) = self.fetch(key)? else {
            return Ok(None);
        };
        match got.template {
            // A leaf template. An address that ran past it is walked into
            // what it rendered — an object by key, an array by index.
            Some(JsonValue::String(t)) => {
                let value = self.render(&t, key)?;
                Ok(match got.rest {
                    Some(rest) => walk(value, &rest),
                    None => Some(value),
                })
            }
            // A group. Every string leaf renders, the shape is kept, and the
            // result wins over the cached data it shares an address with.
            Some(JsonValue::Object(tree)) => {
                let rendered = self.render_tree(&tree, key)?;
                Ok(Some(JsonValue::Object(match got.data {
                    Some(JsonValue::Object(mut data)) => {
                        deep_merge(&mut data, rendered);
                        data
                    }
                    _ => rendered,
                })))
            }
            Some(_) => Err(ResolveError::Config(format!(
                "{key}: template is neither a string nor an object"
            ))),
            None => Ok(got.data),
        }
    }

    /// Ask the daemon for one node, at most once per `(key, path)`.
    fn fetch(&mut self, key: &str) -> Result<Option<Got>, ResolveError> {
        let coord = (key.to_string(), self.inputs.path().map(str::to_string));
        if let Some(got) = self.memo.get(&coord) {
            return Ok(got.clone());
        }
        let got = self
            .fetch
            .get(key, coord.1.as_deref(), self.inputs.force, self.inputs.wait)
            .map_err(ResolveError::Fetch)?;
        self.memo.insert(coord.clone(), got.clone());
        self.order.push(coord);
        Ok(got)
    }

    /// Render one template source, as the node addressed by `key`.
    fn render(&mut self, src: &str, key: &str) -> Result<JsonValue, ResolveError> {
        self.push(key)?;
        let rendered = self.render_frame(src, key);
        self.stack.pop();
        rendered
    }

    fn render_frame(&mut self, src: &str, key: &str) -> Result<JsonValue, ResolveError> {
        let context = self.bind(&discover_refs_deep(src))?;
        // The two forms of `crate::eval::classify`: exactly one `{{ expr }}`
        // takes the typed path, everything else (literal text included)
        // renders to a string.
        match single_tag_expression(src) {
            Some(expr) => {
                let env = build_expression_env(self.inputs.allow_exec);
                let compiled = env.compile_expression(expr).map_err(|e| {
                    ResolveError::Config(format!("{key}: expression compile error: {e}"))
                })?;
                let value = compiled
                    .eval(MjValue::from_serialize(&context))
                    .map_err(|e| {
                        ResolveError::Runtime(format!("{key}: expression eval error: {e}"))
                    })?;
                // An all-empty cascade is `""` — `mj_to_json` maps an
                // undefined or none result onto it (canon invariant 9).
                Ok(mj_to_json(value))
            }
            // The compile/render split is what separates a source someone has
            // to go and fix from a failure at render time; `crate::eval`'s
            // ERROR-MESSAGE ABI CONTRACT pins these two prefixes for exactly
            // this kind of classification.
            None => render_template_with(src, &context, self.inputs.allow_exec)
                .map(JsonValue::String)
                .map_err(|e| {
                    if e.starts_with("template compile error") {
                        ResolveError::Config(format!("{key}: {e}"))
                    } else {
                        ResolveError::Runtime(format!("{key}: {e}"))
                    }
                }),
        }
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
            let leaf = format!("{key}.{name}");
            match node {
                JsonValue::String(src) => {
                    match self.render(src, &leaf) {
                        Ok(value) => {
                            out.insert(name.clone(), value);
                        }
                        // A leaf that failed at render time is omitted; one
                        // that does not compile, cycles or could not be
                        // fetched fails the whole query, already named.
                        Err(ResolveError::Runtime(_)) => {}
                        Err(e) => return Err(e),
                    }
                }
                JsonValue::Object(sub) => {
                    let value = self.render_tree(sub, &leaf)?;
                    out.insert(name.clone(), JsonValue::Object(value));
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
    /// A miss binds nothing — not the value, and not the object that would
    /// have enclosed it — so `default` and `or` fire on it. Where a whole
    /// `cache.P` and a `cache.P.F` disagree, the whole object wins: it is the
    /// authoritative snapshot, whichever ref is bound first.
    fn bind(&mut self, refs: &[Ref]) -> Result<JsonValue, ResolveError> {
        let mut top = Map::new();
        top.insert("env".to_string(), JsonValue::Object(Map::new()));
        top.insert("cache".to_string(), JsonValue::Object(Map::new()));

        for r in refs {
            match r {
                Ref::Env(var) => {
                    let Some(value) = self.inputs.env.get(var).cloned() else {
                        continue;
                    };
                    if let Some(JsonValue::Object(env)) = top.get_mut("env") {
                        env.insert(var.clone(), JsonValue::String(value));
                    }
                }
                Ref::CacheField(provider, field) => {
                    let Some(value) = self
                        .fetch(&format!("{provider}.{field}"))?
                        .and_then(|g| g.data)
                    else {
                        continue;
                    };
                    if let Some(JsonValue::Object(cache)) = top.get_mut("cache") {
                        let entry = cache
                            .entry(provider.clone())
                            .or_insert_with(|| JsonValue::Object(Map::new()));
                        if let JsonValue::Object(fields) = entry {
                            fields.entry(field.clone()).or_insert(value);
                        }
                    }
                }
                Ref::CacheProvider(provider) => {
                    let Some(value) = self.fetch(provider)?.and_then(|g| g.data) else {
                        continue;
                    };
                    if let Some(JsonValue::Object(cache)) = top.get_mut("cache") {
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
                // attribute walk (`scanline.widgets.b`) finds it.
                Ref::Resolved(provider, rest) => {
                    let key = format!("{provider}.{rest}");
                    let Some(value) = self.resolve(&key)? else {
                        continue;
                    };
                    insert_at(&mut top, &key, value);
                }
            }
        }
        Ok(JsonValue::Object(top))
    }

    /// Enter `key`'s frame, refusing a repeat and a chain past the depth limit.
    fn push(&mut self, key: &str) -> Result<(), ResolveError> {
        if self.stack.len() >= MAX_VIRTUAL_DEPTH {
            return Err(ResolveError::Config(too_deep()));
        }
        if self.on_stack(key) {
            return Err(ResolveError::Config(format!(
                "{key}: virtual field cycle detected: {key} references itself"
            )));
        }
        self.stack
            .push((key.to_string(), self.inputs.path().map(str::to_string)));
        Ok(())
    }

    fn on_stack(&self, key: &str) -> bool {
        let path = self.inputs.path();
        self.stack
            .iter()
            .any(|(k, p)| k == key && p.as_deref() == path)
    }
}

/// Walk `rest` into a rendered value: an object by key, an array by decimal
/// index. A scalar reached with address left over, or a key or index that is
/// not there, is a miss.
fn walk(value: JsonValue, rest: &str) -> Option<JsonValue> {
    let mut current = value;
    for segment in rest.split('.') {
        current = match current {
            JsonValue::Object(mut fields) => fields.remove(segment)?,
            JsonValue::Array(mut items) => {
                let index: usize = segment.parse().ok()?;
                (index < items.len()).then(|| items.swap_remove(index))?
            }
            _ => return None,
        };
    }
    Some(current)
}

/// Insert `value` at the dotted `path` under `map`, creating the objects
/// between. A scalar already sitting on the path is left alone — a field of a
/// scalar is not navigable either way.
fn insert_at(map: &mut Map<String, JsonValue>, path: &str, value: JsonValue) {
    match path.split_once('.') {
        None => {
            map.insert(path.to_string(), value);
        }
        Some((head, tail)) => {
            let entry = map
                .entry(head.to_string())
                .or_insert_with(|| JsonValue::Object(Map::new()));
            if let JsonValue::Object(inner) = entry {
                insert_at(inner, tail, value);
            }
        }
    }
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
