//! A provider's own lines: `template`, `result` and `return`.
//!
//! `[daemon.providers.<p>]` takes the three as jinja lines, and an absent one
//! comes whole from `[daemon.providers._]`, the abstract provider
//! `defaults/00-comb.toml` writes.
//!
//! - `template` is read when the facets are seeded, once per configuration
//!   layer, over that layer's `[client]` tree. What it gives is the provider's
//!   `<p>:template` records.
//! - `result` is read when the provider's records are read, once per directory
//!   layer, over the provider's own Sources (`sources`). What it gives is that
//!   layer's Source records.
//! - `return` is read when a `get` is answered, once, over the data read
//!   (`result`) and the `template` facet at the requested path. What it gives
//!   is what the provider answers; the facets it attaches with
//!   `facet(name, obj)` ride beside the data, and only `template` is consumed.
//!
//! `result` and `return` may also call another provider's Source,
//! `daemon.providers.<p>.sources.<s>()`. The lines render without IO, so the
//! call is answered from what is stored; the read that renders them first
//! warms what they call.
//!
//! `| super` pipes a value into the line one step up the chain, and is
//! meaningful on `return` alone: a `template` or `result` line is the whole
//! value, and takes nothing piped in.
//!
//! The abstract provider is read like any other table: from the configuration
//! as composed, the managed `defaults/` layer included. A configuration with
//! no such layer has no `template` and no `return` line, so no `:template`
//! facet is seeded or attached; its `result` is the one line the code
//! supplies, `{{ merge(sources) }}` ([`DEFAULT_RESULT`]).
//!
//! The shipped lines are what nearly every read runs, so the renderer is
//! skipped for them: a `result` that is the default merge is the per-Source
//! merge done natively, and a `return` that is the identity (with or without
//! the `template` facet) hands the data on as it is. Both are the same answer
//! the lines would give.
//!
//! Schedule decorators (`poll`, `watch`, ...) belong to a Source's `return`; on
//! a provider's line they are a load error. So are `exec`, `read`, `lup`, `url`
//! and `plugin`: these lines run on a read, which does no IO.

use crate::cache::{
    COMPUTED_PREFIX, Cache, Layers, MergedRead, RecordFreshness, merge_layer, merge_read,
    note_records, walk_leaves,
};
use crate::engine::{CallFn, Calls, FACET_SINK, FacetSink, facet_name_error, provider_engine_env};
use crate::provider::Value;
use crate::provider::registry::ProviderRegistry;
use crate::provider::source_call::source_call;
use crate::provider::source_call::{CallOutcome, Called, CalledRecord, called_in};
use crate::shape::decorators::named_decorators;
use crate::shape::prototype::{Kind, Link, chain, super_chain};
use dashmap::DashSet;
use libbeachcomber::engine::op_error;
use libbeachcomber::eval::{
    Known, Line, Probe, Use, evaluate, lint, single_tag_expression, undeclared_names, used_names,
};
use libbeachcomber::library::{Surface, first_refused};
use minijinja::value::{Object, ObjectRepr, Value as MjValue};
use minijinja::{Environment, Error};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::time::Instant;

/// The `result` of a provider whose chain has no line: the merge of its
/// Sources. The one line the code supplies; every other comes from a file.
pub const DEFAULT_RESULT: &str = "{{ merge(sources) }}";

/// The `result` of a provider with `enabled = false`: it produces nothing.
pub const DISABLED_RESULT: &str = "{{ {} }}";

/// The `return` of a provider with `enabled = false`: the data as it is, and
/// no `template` facet beside it, so no template half rides along on a `get`.
/// (The `<p>:template` facet is still seeded and readable by its own name.)
pub const DISABLED_RETURN: &str = "{{ result }}";

/// Whether `providers` (a composed `[daemon.providers]`) switch `name` off. On
/// `_` it is a file error, so no prototype holds one.
pub fn is_disabled_in(providers: &toml::Table, name: &str) -> bool {
    name != "_"
        && providers
            .get(name)
            .and_then(|p| p.get("enabled"))
            .and_then(toml::Value::as_bool)
            == Some(false)
}

/// How many distinct render failures are remembered, so each is logged once. A
/// name no `[daemon.providers.<p>]` table owns can fail too, and the set must
/// not grow with the names a client asks for.
const MAX_LOGGED: usize = 256;

/// A line of a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Template,
    Result,
    Return,
}

impl Key {
    /// The three keys, in the order a file lists them.
    pub const ALL: [Key; 3] = [Key::Template, Key::Result, Key::Return];

    /// The key as the file spells it.
    pub fn name(self) -> &'static str {
        match self {
            Key::Template => "template",
            Key::Result => "result",
            Key::Return => "return",
        }
    }

    fn index(self) -> usize {
        match self {
            Key::Template => 0,
            Key::Result => 1,
            Key::Return => 2,
        }
    }

    /// The names a line of this key may read; any other is a load error.
    /// On `return`, `result` is also the value piped in by a nearer line's
    /// `| super`.
    /// What kind of line it is, for the names it sees ([`Line::sees`]).
    fn line(self) -> Line {
        match self {
            Key::Template => Line::ProviderTemplate,
            Key::Result => Line::ProviderResult,
            Key::Return => Line::ProviderReturn,
        }
    }
}

/// What a provider's `return` does, told from its effective line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReturnKind {
    /// The data as read, and no facet. No line, and `{{ result }}`.
    #[default]
    Identity,
    /// The data as read, with the `template` facet beside it: the shipped line.
    IdentityWithTemplate,
    /// Any other line: rendered once per `get`.
    Line,
}

/// One line of a chain, and the table that wrote it.
#[derive(Debug, Clone)]
struct Level {
    path: String,
    line: String,
}

/// The lines a provider's `template`, `result` and `return` resolve to, each
/// nearest first: its own, then the abstract provider's. `| super` on a
/// `return` steps down this list.
#[derive(Debug, Default)]
pub struct Chains {
    levels: [Vec<Level>; 3],
    /// What the renderer is handed per key: the whole chain for `return`, the
    /// nearest line alone for the others.
    lines: [Arc<Vec<String>>; 3],
    return_kind: ReturnKind,
    result_default: bool,
    /// `enabled = false`: `result` is the empty one, `return` carries no
    /// template, and no Source is selected, called or run.
    disabled: bool,
    /// Whether any line of the `return` chain reads `result` / `template`.
    return_reads_result: bool,
    return_reads_template: bool,
    /// The other providers' Sources the `result` and `return` lines call,
    /// as `(provider, source)`, each once: together, and by line.
    calls: Vec<(String, String)>,
    /// Whether `calls` is not empty: the one flag a read of a provider that
    /// calls nothing looks at.
    has_calls: bool,
    result_calls: Vec<(String, String)>,
    return_calls: Vec<(String, String)>,
}

impl Chains {
    fn of(&self, key: Key) -> &[Level] {
        &self.levels[key.index()]
    }

    /// The text of `key`'s effective line (its nearest), if any applies.
    pub fn effective(&self, key: Key) -> Option<&str> {
        self.of(key).first().map(|l| l.line.as_str())
    }

    /// Whether `result` is the default merge of the provider's Sources.
    pub fn result_is_default(&self) -> bool {
        self.result_default
    }

    pub fn return_kind(&self) -> ReturnKind {
        self.return_kind
    }

    /// Whether the provider is switched off (`enabled = false`): its `result`
    /// is [`DISABLED_RESULT`], its `return` [`DISABLED_RETURN`], and no Source
    /// of it is selected, called or run.
    pub fn is_disabled(&self) -> bool {
        self.disabled
    }

    /// Whether a read of the provider is the Sources' records as they are
    /// stored: the default `result` and an identity `return`. Then a field is
    /// owned by the Source that declares it; otherwise a line decides what
    /// exists, and any Source may be behind any field.
    pub fn reads_by_default(&self) -> bool {
        self.result_default && self.return_kind != ReturnKind::Line
    }
}

fn links<'t>(providers: &'t toml::Table, name: &str) -> Vec<Link<'t>> {
    if name == "_" {
        return providers
            .get("_")
            .and_then(toml::Value::as_table)
            .map(|table| Link {
                path: "[daemon.providers._]".into(),
                table,
            })
            .into_iter()
            .collect();
    }
    chain(providers, Kind::Provider, name, "")
}

/// The chains of `name` among `providers` (a composed `[daemon.providers]`). A
/// line of the wrong type is the file check's error, and is not in a chain.
fn chains(probe: &Probe<'_>, providers: &toml::Table, name: &str) -> Chains {
    let links = links(providers, name);
    // `enabled` is read from the provider's own table: on `_` it is a file
    // error, so no prototype holds one.
    let disabled = is_disabled_in(providers, name);
    let levels = |key: Key| -> Vec<Level> {
        let mut found: Vec<Level> = super_chain(&links, key.name())
            .into_iter()
            .filter_map(|(at, value)| {
                Some(Level {
                    path: links[at].path.clone(),
                    line: value.as_str()?.to_string(),
                })
            })
            .collect();
        if disabled && matches!(key, Key::Result | Key::Return) {
            // The switch is the nearest `result` and `return`: any other is
            // not read.
            let line = if key == Key::Result {
                DISABLED_RESULT
            } else {
                DISABLED_RETURN
            };
            found.insert(
                0,
                Level {
                    path: links[0].path.clone(),
                    line: line.into(),
                },
            );
        }
        if found.is_empty() && key == Key::Result {
            found.push(Level {
                path: "(built in)".into(),
                line: DEFAULT_RESULT.into(),
            });
        }
        found
    };
    let levels = [
        levels(Key::Template),
        levels(Key::Result),
        levels(Key::Return),
    ];
    let lines_of = |key: Key| -> Arc<Vec<String>> {
        let all = levels[key.index()].iter().map(|l| l.line.clone());
        Arc::new(if key == Key::Return {
            all.collect()
        } else {
            all.take(1).collect()
        })
    };
    let lines = [
        lines_of(Key::Template),
        lines_of(Key::Result),
        lines_of(Key::Return),
    ];
    let reads: BTreeSet<String> = lines[Key::Return.index()]
        .iter()
        .filter_map(|line| undeclared_names(probe, line, false).ok())
        .flatten()
        .collect();
    let calls_of = |key: Key| -> Vec<(String, String)> {
        let found: BTreeSet<(String, String)> = lines[key.index()]
            .iter()
            .flat_map(|line| called_in(probe, line).unwrap_or_default())
            .filter_map(|called| match called {
                Called::Of(provider, source) => Some((provider, source)),
                Called::Sibling(_) => None,
            })
            .collect();
        found.into_iter().collect()
    };
    let result_calls = calls_of(Key::Result);
    let return_calls = calls_of(Key::Return);
    let calls: Vec<(String, String)> = result_calls
        .iter()
        .chain(&return_calls)
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let return_kind = match lines[Key::Return.index()].first() {
        None => ReturnKind::Identity,
        Some(line) => return_kind_of(line),
    };
    let result_default = !disabled
        && lines[Key::Result.index()]
            .first()
            .is_some_and(|l| is_default_result(l));
    Chains {
        levels,
        lines,
        return_kind,
        result_default,
        disabled,
        return_reads_result: reads.contains("result"),
        return_reads_template: reads.contains("template"),
        has_calls: !calls.is_empty(),
        calls,
        result_calls,
        return_calls,
    }
}

/// `line` with its whitespace gone and its quotes made single: what two
/// spellings of the same short line have in common.
fn squeezed(line: &str) -> String {
    line.chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| if c == '"' { '\'' } else { c })
        .collect()
}

/// Whether `line` is the default `result`: `{{ merge(sources) }}`, whitespace
/// aside. Only then do two Sources of a provider owe each other distinct field
/// names; an author who writes the merge has chosen an order.
///
/// A text test, because minijinja exposes no syntax tree to ask "is this
/// exactly `merge(sources)`"; the single tag is told from the text around it
/// by the same rule every other line is.
pub fn is_default_result(line: &str) -> bool {
    single_tag_expression(line).is_some_and(|expr| squeezed(expr) == "merge(sources)")
}

/// The two `return` lines that hand the data on as it is (the second also
/// attaching the `template` facet), told by text for the reason
/// [`is_default_result`] is.
fn return_kind_of(line: &str) -> ReturnKind {
    let Some(expr) = single_tag_expression(line) else {
        return ReturnKind::Line;
    };
    match squeezed(expr).as_str() {
        "result" => ReturnKind::Identity,
        "result|facet('template',template)" => ReturnKind::IdentityWithTemplate,
        _ => ReturnKind::Line,
    }
}

/// What a line gave, and the facets it attached on the way.
#[derive(Debug, Default)]
pub struct Evaluated {
    /// `None`: the line gave none (or an undefined).
    pub value: Option<MjValue>,
    /// `facet(name, obj)` calls in order; the last of a name is its value.
    pub facets: Vec<(String, serde_json::Value)>,
}

/// One of a provider's Sources as a read found it.
pub struct Bound<'a> {
    pub name: &'a str,
    /// The Source's stored map at the entry being read, with its freshness;
    /// `None` when the read did not visit its instance or it holds nothing.
    pub held: Option<(&'a HashMap<String, Value>, RecordFreshness)>,
}

/// What `return` gave: the data the provider answers, and the `template` facet
/// beside it.
#[derive(Debug)]
pub struct Returned {
    pub read: Option<MergedRead>,
    pub template: Option<HashMap<String, Value>>,
}

/// A provider's compiled lines, for one composed configuration. The chains are
/// resolved once per provider the configuration names; every other name shares
/// the abstract provider's.
pub struct ProviderLines {
    /// The engine the lines render in, which they are read in at load too.
    probe: Probe<'static>,
    own: HashMap<String, Arc<Chains>>,
    shared: Arc<Chains>,
    /// Whether any provider's lines call a Source: when none does, a read looks
    /// no further.
    any_calls: bool,
    /// Whether any provider is switched off: when none is, a read looks no
    /// further.
    any_disabled: bool,
    logged: DashSet<String>,
    /// Whether the log said that it stopped remembering failures.
    suppressed: AtomicBool,
}

impl ProviderLines {
    /// The lines `providers` (a composed `[daemon.providers]`) say. Every line
    /// of every table, `_`'s included, is checked here: a schedule decorator,
    /// a call that does IO, a `| super` where it means nothing, a name the line
    /// may not read, or text that does not compile is a load error.
    pub fn new(providers: &toml::Table) -> Result<Self, Vec<String>> {
        let mut env = provider_engine_env(Default::default());
        env.add_filter("super", step_up);
        let probe = Probe::new(env);
        let own: HashMap<String, Arc<Chains>> = providers
            .keys()
            .map(|name| (name.clone(), Arc::new(chains(&probe, providers, name))))
            .collect();
        let shared = Arc::new(chains(&probe, providers, "_"));
        let any_calls = shared.has_calls || own.values().any(|c| c.has_calls);
        let any_disabled = own.values().any(|c| c.disabled);
        let lines = Self {
            probe,
            own,
            shared,
            any_calls,
            any_disabled,
            logged: DashSet::new(),
            suppressed: AtomicBool::new(false),
        };
        let errors = lines.lint();
        if errors.is_empty() {
            Ok(lines)
        } else {
            Err(errors)
        }
    }

    fn lint(&self) -> Vec<String> {
        let mut errors = BTreeSet::new();
        for chains in self.own.values().chain(std::iter::once(&self.shared)) {
            for key in Key::ALL {
                for Level { path, line } in chains.of(key) {
                    let at = format!("{path} {}", key.name());
                    if let Some(decorator) = named_decorators(line).first() {
                        errors.insert(format!(
                            "{at}: {decorator}() is a schedule decorator: it belongs on a \
                             Source's return, not a provider's line"
                        ));
                        continue;
                    }
                    let used = used_names(line);
                    if let Some(item) = first_refused(Surface::Provider, |n| used.contains_key(n)) {
                        let name = item.name;
                        errors.insert(format!(
                            "{at}: {name} is not available in a provider's line: these lines \
                             run on every read and do no IO"
                        ));
                        continue;
                    }
                    if key != Key::Return && used.get("super") == Some(&Use::Filter) {
                        errors.insert(format!(
                            "{at}: | super means nothing on a provider's {} line, which is the \
                             whole value and takes nothing piped in: write the whole line, or \
                             pipe into a `return`",
                            key.name()
                        ));
                        continue;
                    }
                    if let Some(message) = literal_facet_names(line)
                        .into_iter()
                        .find_map(facet_name_error)
                    {
                        errors.insert(format!("{at}: {message}"));
                        continue;
                    }
                    match called_in(&self.probe, line) {
                        Err(e) => {
                            errors.insert(format!("{at}: {e}"));
                            continue;
                        }
                        Ok(called) if *path == "[daemon.providers._]" => {
                            if let Some(Called::Sibling(source)) =
                                called.iter().find(|c| matches!(c, Called::Sibling(_)))
                            {
                                errors.insert(format!(
                                    "{at}: sources.{source}(): the abstract provider applies to \
                                     every provider, so it cannot name one of their Sources: use \
                                     merge(sources)"
                                ));
                                continue;
                            }
                        }
                        Ok(_) => {}
                    }
                    for finding in lint(&self.probe, line, key.line(), &Known::default()) {
                        errors.insert(format!("{at}: {finding}"));
                    }
                }
            }
        }
        errors.into_iter().collect()
    }

    /// Every Source a `result` or `return` line calls, with the line's place
    /// (`[daemon.providers.p] result`) and, for a call to `sources.<s>()`, the
    /// provider whose own table the line is in: for the load to check against
    /// the Sources that exist.
    pub fn calls(&self) -> Vec<(String, Option<String>, Called)> {
        let mut found = BTreeSet::new();
        let owned = self.own.iter().map(|(n, c)| (Some(n.as_str()), c));
        for (name, chains) in owned.chain(std::iter::once((None, &self.shared))) {
            for key in [Key::Result, Key::Return] {
                for Level { path, line } in chains.of(key) {
                    let owner = name
                        .filter(|n| *n != "_" && *path == format!("[daemon.providers.{n}]"))
                        .map(str::to_string);
                    for called in called_in(&self.probe, line).unwrap_or_default() {
                        found.insert((format!("{path} {}", key.name()), owner.clone(), called));
                    }
                }
            }
        }
        found.into_iter().collect()
    }

    /// The other providers' Sources `provider`'s `result` line calls, as
    /// `(provider, source)`.
    pub fn result_calls(&self, provider: &str) -> &[(String, String)] {
        &self.own.get(provider).unwrap_or(&self.shared).result_calls
    }

    /// Whether `provider`'s `result` or `return` line calls any Source.
    pub fn has_calls(&self, provider: &str) -> bool {
        self.any_calls && self.own.get(provider).unwrap_or(&self.shared).has_calls
    }

    /// The other providers' Sources `provider`'s `result` and `return` lines
    /// call: what a read brings up to date before the lines render.
    pub fn all_calls(&self, provider: &str) -> &[(String, String)] {
        &self.own.get(provider).unwrap_or(&self.shared).calls
    }

    /// Whether `provider` has `enabled = false`.
    pub fn is_disabled(&self, provider: &str) -> bool {
        self.any_disabled && self.own.get(provider).is_some_and(|c| c.disabled)
    }

    /// The resolved chains of `provider`: its own when the configuration has a
    /// table for it, the abstract provider's otherwise.
    pub fn chains_for(&self, provider: &str) -> Arc<Chains> {
        self.own.get(provider).unwrap_or(&self.shared).clone()
    }

    /// Render `provider`'s `key` line against `vars`, plus `_.name`. On a
    /// `return`, a `| super` renders the line one step up with its input as
    /// `result`. An absent line gives none.
    pub fn eval(
        &self,
        provider: &str,
        key: Key,
        vars: Vec<(&'static str, MjValue)>,
    ) -> Result<Evaluated, String> {
        let lines = self.chains_for(provider).lines[key.index()].clone();
        if lines.is_empty() {
            return Ok(Evaluated::default());
        }
        let mut vars: Vec<(String, MjValue)> =
            vars.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        vars.push((
            "_".into(),
            MjValue::from_serialize(serde_json::json!({"name": provider})),
        ));
        let sink = MjValue::from_object(FacetSink::default());
        let value = render_level(self.probe.env(), &lines, 0, &Arc::new(vars), &sink)
            .map_err(|e| format!("{provider} {}: {e}", key.name()))?;
        let facets = sink
            .downcast_object_ref::<FacetSink>()
            .map(FacetSink::take)
            .unwrap_or_default();
        Ok(Evaluated { value, facets })
    }

    /// The `<p>:template` records `provider`'s `template` line gives over one
    /// layer's `[client]` tree, `client`, as the engine reads it (made once
    /// per layer, for every provider): `Ok(None)` when it gives none, an error
    /// when what it gives is not a table of value expressions.
    pub fn template_tree(
        &self,
        provider: &str,
        client: &MjValue,
    ) -> Result<Option<serde_json::Value>, String> {
        let vars = vec![("client", client.clone())];
        let Some(value) = self.eval(provider, Key::Template, vars)?.value else {
            return Ok(None);
        };
        let tree = serde_json::to_value(&value).map_err(|e| e.to_string())?;
        match check_tree(&tree, "") {
            Ok(()) if tree.is_object() => Ok(Some(tree)),
            Ok(()) => Err(format!("{provider} template: gave a value, not a table")),
            Err(at) => Err(format!(
                "{provider} template: '{at}' is not a value expression or a group"
            )),
        }
    }

    /// Note that a line failed to render, logging `message` the first time it
    /// is seen. A message names its provider and key, so this is once per
    /// (provider, key, message): a later, different failure is logged too.
    /// Returns whether this was the first.
    pub fn note_error(&self, message: &str) -> bool {
        if self.logged.len() >= MAX_LOGGED && !self.logged.contains(message) {
            if !self.suppressed.swap(true, Ordering::Relaxed) {
                tracing::error!("further provider-line failures suppressed");
            }
            return false;
        }
        let first = self.logged.insert(message.to_string());
        if first {
            tracing::error!("{message}");
        }
        first
    }

    #[cfg(test)]
    fn suppressed_notice_given(&self) -> bool {
        self.suppressed.load(Ordering::Relaxed)
    }

    /// Merge `provider`'s records at one entry into `out`: its `result` over
    /// the Sources `bound` found there. Returns whether anything merged.
    ///
    /// The default `result` is the old merge, done natively in declaration
    /// order straight into `out`: each Source's leaves keep that Source's
    /// freshness. Any other line is rendered, and each leaf it gives takes the
    /// freshness of the one Source holding that very value there; a leaf no
    /// single Source holds is computed, and is as old as the oldest input
    /// behind it.
    pub fn assemble(
        &self,
        provider: &str,
        at: Option<&str>,
        bound: &[Bound<'_>],
        call: &dyn Fn(&str, &str) -> CallOutcome,
        out: &mut MergedRead,
    ) -> Result<bool, String> {
        if self.chains_for(provider).result_default {
            let mut any = false;
            for (fields, fresh) in bound.iter().filter_map(|b| b.held.as_ref()) {
                merge_layer(&mut out.fields, &mut out.records, fields, fresh, "");
                any = true;
            }
            return Ok(any);
        }
        let calls = bound
            .iter()
            .map(|b| {
                (
                    b.name.to_string(),
                    b.held
                        .as_ref()
                        .and_then(|(fields, _)| serde_json::to_value(fields).ok()),
                )
            })
            .collect();
        let sources = MjValue::from_object(crate::engine::Sources::new(calls));
        let mut vars = vec![("sources", sources)];
        let called = self.result_calls(provider);
        let used = Used::default();
        if !called.is_empty() {
            vars.push(("daemon", daemon_of(called, call, &used)));
        }
        let evaluated = self.eval(provider, Key::Result, vars)?;
        let Some(value) = evaluated.value else {
            return Ok(false);
        };
        let Some(Value::Object(fields)) = serde_json::to_value(&value)
            .ok()
            .as_ref()
            .and_then(Value::from_json)
        else {
            return Err(format!(
                "{provider} result: a provider's result is a map of fields"
            ));
        };
        if fields.is_empty() {
            return Ok(false);
        }
        let mut inputs: Vec<Input> = bound
            .iter()
            .filter_map(|b| {
                let (held, fresh) = b.held.as_ref()?;
                let mut records = HashMap::new();
                for (k, v) in *held {
                    note_records(&mut records, k, v, fresh);
                }
                Some(Input::new(held, records))
            })
            .collect();
        inputs.extend(used.inputs());
        let records = attribute(&fields, &inputs, at, "result");
        merge_read(&mut out.fields, &mut out.records, &fields, &records, "");
        Ok(true)
    }

    /// What `provider` answers: its `return` over `result`, the data its
    /// records gave, and the `template` facet (read only when the line reads
    /// it). The identity lines are not rendered.
    pub fn returned(
        &self,
        provider: &str,
        result: Option<MergedRead>,
        template: impl FnOnce() -> Option<HashMap<String, Value>>,
        call: &dyn Fn(&str, &str) -> CallOutcome,
    ) -> Returned {
        let chains = self.chains_for(provider);
        match chains.return_kind {
            ReturnKind::Identity => {
                return Returned {
                    read: result,
                    template: None,
                };
            }
            ReturnKind::IdentityWithTemplate => {
                return Returned {
                    read: result,
                    template: template(),
                };
            }
            ReturnKind::Line => {}
        }
        let as_value = |fields: Option<&HashMap<String, Value>>| {
            fields.map_or_else(|| MjValue::from(()), MjValue::from_serialize)
        };
        let mut vars = Vec::new();
        if chains.return_reads_result {
            vars.push(("result", as_value(result.as_ref().map(|r| &r.fields))));
        }
        if chains.return_reads_template {
            vars.push(("template", as_value(template().as_ref())));
        }
        let used = Used::default();
        if !chains.return_calls.is_empty() {
            vars.push(("daemon", daemon_of(&chains.return_calls, call, &used)));
        }
        let prepared = self.eval(provider, Key::Return, vars);
        let evaluated = match prepared {
            Ok(evaluated) => evaluated,
            Err(message) => {
                self.note_error(&message);
                return Returned {
                    read: None,
                    template: None,
                };
            }
        };
        let attached = evaluated
            .facets
            .into_iter()
            .rev()
            .find(|(name, _)| name == "template")
            .and_then(|(_, tree)| match Value::from_json(&tree) {
                Some(Value::Object(fields)) => Some(fields),
                _ => None,
            });
        let read = match evaluated.value {
            None => None,
            Some(value) => match serde_json::to_value(&value)
                .ok()
                .as_ref()
                .and_then(Value::from_json)
            {
                Some(Value::Object(fields)) if fields.is_empty() => None,
                Some(Value::Object(fields)) => {
                    let mut inputs: Vec<Input> = result
                        .iter()
                        .map(|r| Input::new(&r.fields, r.records.clone()))
                        .collect();
                    inputs.extend(used.inputs());
                    let records = attribute(&fields, &inputs, None, "return");
                    Some(MergedRead { fields, records })
                }
                _ => {
                    self.note_error(&format!(
                        "{provider} return: a provider's return is a map of fields"
                    ));
                    return Returned {
                        read: None,
                        template: None,
                    };
                }
            },
        };
        Returned {
            read,
            template: attached,
        }
    }
}

/// What `provider` answers over `result`, the data its records gave at
/// `path`: its `return`, with the `provider:template` facet read at `path`
/// only if the line reads it. The one place the data half of an answer is
/// made, for a `get`, a `watch` frame, `p:cache` and a Source's `comb.<p>`.
pub fn returned_over(
    cache: &Cache,
    registry: &ProviderRegistry,
    provider: &str,
    path: Option<&str>,
    result: Option<MergedRead>,
) -> Returned {
    registry.lines().returned(
        provider,
        result,
        || {
            cache
                .read_merged(
                    &format!("{provider}:template"),
                    path,
                    &[],
                    registry,
                    Layers::All,
                )
                .map(|facet| facet.fields)
        },
        &|p, s| source_call(registry, cache, p, s, path),
    )
}

/// The records of the Sources a line called, as it called them.
#[derive(Clone, Default)]
struct Used(Arc<std::sync::Mutex<Vec<CalledRecord>>>);

impl Used {
    /// Each as an input to the line's output: only its age counts.
    fn inputs(&self) -> Vec<Input> {
        let used = self.0.lock().unwrap_or_else(|e| e.into_inner());
        used.iter().map(Input::called).collect()
    }
}

/// `daemon`, over the Sources a line calls. Their records are read now, stored
/// only (the line renders on the read path and runs nothing); a read that
/// failed fails the line only if the line calls it.
fn daemon_of(
    called: &[(String, String)],
    call: &dyn Fn(&str, &str) -> CallOutcome,
    used: &Used,
) -> MjValue {
    let records: HashMap<(String, String), CallOutcome> = called
        .iter()
        .map(|(p, s)| ((p.clone(), s.clone()), call(p, s)))
        .collect();
    let used = used.clone();
    let answer: CallFn = Arc::new(
        move |p, s| match records.get(&(p.to_string(), s.to_string())) {
            Some(Ok(Some(record))) => {
                used.0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(record.clone());
                Ok(Some(record.value.clone()))
            }
            Some(Err(e)) => Err(e.clone()),
            _ => Ok(None),
        },
    );
    Calls::daemon(answer)
}

/// The facet names `line` writes as a literal first argument of `facet(...)`,
/// so a name outside the set is a load error where it is written. A text
/// scan, like [`named_decorators`]; a name computed in the line is checked
/// when it renders.
fn literal_facet_names(line: &str) -> Vec<&str> {
    let mut found = Vec::new();
    for (at, _) in line.match_indices("facet") {
        if line[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_' || c == '.') {
            continue;
        }
        let Some(args) = line[at + "facet".len()..].trim_start().strip_prefix('(') else {
            continue;
        };
        let args = args.trim_start();
        let Some(quote) = args.chars().next().filter(|c| matches!(c, '\'' | '"')) else {
            continue;
        };
        if let Some(end) = args[1..].find(quote) {
            found.push(&args[1..1 + end]);
        }
    }
    found
}

/// One input to a line, for telling which Source a leaf it gave came from.
struct Input {
    leaves: HashMap<String, Value>,
    records: HashMap<String, RecordFreshness>,
    /// The record last refreshed longest ago.
    oldest: Option<RecordFreshness>,
}

impl Input {
    /// A called Source's record: it holds no leaf of its own to attribute, but
    /// what a line computes from it is as old as it is.
    fn called(record: &CalledRecord) -> Self {
        let mut leaves = Vec::new();
        if let Some(Value::Object(fields)) = Value::from_json(&record.value) {
            walk_leaves("", Value::Object(fields), &mut leaves);
        }
        Self {
            leaves: leaves.into_iter().collect(),
            records: HashMap::new(),
            oldest: Some(record.fresh.clone()),
        }
    }

    fn new(fields: &HashMap<String, Value>, records: HashMap<String, RecordFreshness>) -> Self {
        let mut leaves = Vec::new();
        walk_leaves("", Value::Object(fields.clone()), &mut leaves);
        let oldest = records.values().min_by_key(|r| r.last_refreshed).cloned();
        Self {
            leaves: leaves.into_iter().collect(),
            records,
            oldest,
        }
    }
}

/// The freshness of each leaf of `fields`, a line's output over `inputs`. A
/// leaf exactly one input holds with that very value is that input's record.
/// Any other is computed by the line: labelled as such (a static label, with no
/// Source or schedule behind it) and as old as the oldest of the inputs that
/// hold it, or of all of them when none does.
fn attribute(
    fields: &HashMap<String, Value>,
    inputs: &[Input],
    at: Option<&str>,
    line: &str,
) -> HashMap<String, RecordFreshness> {
    let mut leaves = Vec::new();
    walk_leaves("", Value::Object(fields.clone()), &mut leaves);
    let label: Arc<str> = Arc::from(format!("{COMPUTED_PREFIX}{line}"));
    let entry: Option<Arc<str>> = at.map(Arc::from);
    let computed = |pool: &[&Input]| {
        let oldest = pool
            .iter()
            .filter_map(|i| i.oldest.as_ref())
            .min_by_key(|r| r.last_refreshed);
        RecordFreshness {
            source: label.clone(),
            entry: entry.clone(),
            last_refreshed: oldest.map_or_else(Instant::now, |r| r.last_refreshed),
            expected_interval_secs: oldest.and_then(|r| r.expected_interval_secs),
        }
    };
    let all: Vec<&Input> = inputs.iter().collect();
    let mut records = HashMap::new();
    for (tail, value) in leaves {
        let holders: Vec<&Input> = inputs
            .iter()
            .filter(|i| i.leaves.get(&tail) == Some(&value))
            .collect();
        let record = match holders.as_slice() {
            [only] => only.records.get(&tail).cloned(),
            _ => None,
        }
        .unwrap_or_else(|| computed(if holders.len() > 1 { &holders } else { &all }));
        records.insert(tail, record);
    }
    records
}

/// A rendered template tree is tables and string leaves, with non-empty keys.
/// A key may hold a `.`: it is one name, addressed as a quoted segment.
/// `Err` is the address of the first thing that is not.
fn check_tree(value: &serde_json::Value, at: &str) -> Result<(), String> {
    match value {
        serde_json::Value::String(_) => Ok(()),
        serde_json::Value::Object(map) => {
            for (key, sub) in map {
                let here = libbeachcomber::key::push(at, key);
                if key.is_empty() {
                    return Err(here);
                }
                check_tree(sub, &here)?;
            }
            Ok(())
        }
        _ => Err(at.to_string()),
    }
}

/// The line one step up the chain, bound by `| super`.
#[derive(Debug)]
struct Above {
    lines: Arc<Vec<String>>,
    level: usize,
    vars: Arc<Vec<(String, MjValue)>>,
    sink: MjValue,
}

impl Object for Above {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Plain
    }
}

/// The context name the line above is bound to.
const ABOVE: &str = "super__";

/// `value | super`: render the line above with `value` as its `result`.
fn step_up(state: &minijinja::State, input: MjValue) -> Result<MjValue, Error> {
    let above = state.lookup(ABOVE);
    let Some(above) = above
        .as_ref()
        .and_then(|a| a.downcast_object_ref::<Above>())
    else {
        return Err(op_error("`| super` has nothing above it"));
    };
    let mut vars: Vec<(String, MjValue)> = above
        .vars
        .iter()
        .filter(|(name, _)| name != "result")
        .cloned()
        .collect();
    vars.push(("result".into(), input));
    let rendered = render_level(
        state.env(),
        &above.lines,
        above.level,
        &Arc::new(vars),
        &above.sink,
    )
    .map_err(op_error)?;
    Ok(rendered.unwrap_or_else(|| MjValue::from(())))
}

/// Render `lines[level]` over `vars`; `None` when it gives none.
fn render_level(
    env: &Environment<'_>,
    lines: &Arc<Vec<String>>,
    level: usize,
    vars: &Arc<Vec<(String, MjValue)>>,
    sink: &MjValue,
) -> Result<Option<MjValue>, String> {
    let mut context: Vec<(String, MjValue)> = vars.iter().cloned().collect();
    context.push((FACET_SINK.into(), sink.clone()));
    if level + 1 < lines.len() {
        context.push((
            ABOVE.into(),
            MjValue::from_object(Above {
                lines: lines.clone(),
                level: level + 1,
                vars: vars.clone(),
                sink: sink.clone(),
            }),
        ));
    }
    evaluate(env, &lines[level], MjValue::from_iter(context)).map_err(|e| e.message().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Sources;
    use serde_json::json;

    fn table(text: &str) -> toml::Table {
        toml::from_str(text).unwrap()
    }

    /// `[daemon.providers]` as the shipped `00-comb.toml` writes it, with
    /// `text` (a `[daemon.providers]` body) merged over: the managed defaults
    /// layer under a user's lines, composed as the daemon composes them.
    fn over_shipped(text: &str) -> toml::Table {
        let (_, shipped) = crate::daemon::defaults::comb_layer();
        let mut out = shipped["daemon"]["providers"].clone();
        crate::config::deep_merge_toml(&mut out, toml::Value::Table(table(text)));
        match out {
            toml::Value::Table(t) => t,
            _ => unreachable!(),
        }
    }

    fn lines(text: &str) -> ProviderLines {
        ProviderLines::new(&over_shipped(text)).unwrap_or_else(|e| panic!("loads: {e:?}"))
    }

    fn errors(text: &str) -> Vec<String> {
        match ProviderLines::new(&over_shipped(text)) {
            Ok(_) => Vec::new(),
            Err(e) => e,
        }
    }

    fn sources(records: &[(&str, Option<serde_json::Value>)]) -> MjValue {
        MjValue::from_object(Sources::new(
            records
                .iter()
                .map(|(name, record)| (name.to_string(), record.clone()))
                .collect(),
        ))
    }

    fn result(lines: &ProviderLines, provider: &str, records: MjValue) -> serde_json::Value {
        let value = lines
            .eval(provider, Key::Result, vec![("sources", records)])
            .unwrap()
            .value;
        value.map_or(serde_json::Value::Null, |v| {
            serde_json::to_value(&v).unwrap()
        })
    }

    #[test]
    fn the_abstract_provider_is_what_the_defaults_layer_writes() {
        let chains = lines("").chains_for("git");
        assert_eq!(
            chains.effective(Key::Template),
            Some("{{ client.providers[_.name].template }}")
        );
        assert_eq!(chains.effective(Key::Result), Some("{{ merge(sources) }}"));
        assert_eq!(
            chains.effective(Key::Return),
            Some("{{ result | facet('template', template) }}")
        );
    }

    #[test]
    fn an_own_line_is_the_whole_value_and_an_absent_one_is_the_abstracts() {
        let git = lines("[git]\nreturn = \"{{ result }}\"").chains_for("git");
        assert_eq!(git.effective(Key::Return), Some("{{ result }}"));
        assert_eq!(git.effective(Key::Result), Some("{{ merge(sources) }}"));
        assert_eq!(git.of(Key::Return).len(), 2, "super reaches the abstract");
    }

    #[test]
    fn the_default_merge_calls_every_source_in_order() {
        let lines = lines("");
        let got = result(
            &lines,
            "demo",
            sources(&[
                ("one", Some(json!({"a": 1, "shared": {"x": 1}}))),
                ("none", None),
                ("two", Some(json!({"b": 2, "shared": {"y": 2}}))),
            ]),
        );
        assert_eq!(got, json!({"a": 1, "b": 2, "shared": {"x": 1, "y": 2}}));
    }

    #[test]
    fn a_provider_with_no_sources_answers_nothing() {
        let got = result(&lines(""), "bare", sources(&[]));
        assert_eq!(got, serde_json::Value::Null, "an empty record, no hit");
    }

    #[test]
    fn no_defaults_layer_leaves_no_template_or_return_and_a_code_default_result() {
        let bare = ProviderLines::new(&toml::Table::new()).unwrap();
        let chains = bare.chains_for("git");
        assert_eq!(chains.effective(Key::Template), None);
        assert_eq!(chains.effective(Key::Return), None);
        assert_eq!(chains.effective(Key::Result), Some(DEFAULT_RESULT));
        let got = result(
            &bare,
            "git",
            sources(&[
                ("one", Some(json!({"a": 1}))),
                ("two", Some(json!({"b": 2}))),
            ]),
        );
        assert_eq!(got, json!({"a": 1, "b": 2}));
    }

    #[test]
    fn explicit_merge_last_wins() {
        let lines = lines("[demo]\nresult = \"{{ merge(sources.a(), sources.b()) }}\"");
        let got = result(
            &lines,
            "demo",
            sources(&[
                ("a", Some(json!({"x": "a", "keep": 1}))),
                ("b", Some(json!({"x": "b"}))),
            ]),
        );
        assert_eq!(got, json!({"x": "b", "keep": 1}));
        let got = result(
            &lines,
            "demo",
            sources(&[("a", Some(json!({"x": "a"}))), ("b", None)]),
        );
        assert_eq!(got, json!({"x": "a"}), "a Source with nothing adds nothing");
    }

    #[test]
    fn name_is_the_provider_the_line_is_applied_to() {
        let lines = lines("");
        let client =
            MjValue::from_serialize(json!({"providers": {"aws": {"template": {"p": "x"}}}}));
        assert_eq!(
            lines.template_tree("aws", &client).unwrap(),
            Some(json!({"p": "x"}))
        );
        assert_eq!(lines.template_tree("gcp", &client).unwrap(), None);
    }

    #[test]
    fn a_template_line_may_read_another_providers_client_tree() {
        let lines = lines("[git]\ntemplate = \"{{ client.providers.aws.template }}\"");
        let client =
            MjValue::from_serialize(json!({"providers": {"aws": {"template": {"p": "x"}}}}));
        assert_eq!(
            lines.template_tree("git", &client).unwrap(),
            Some(json!({"p": "x"}))
        );
    }

    #[test]
    fn a_template_line_that_gives_a_non_table_is_an_error() {
        let lines = lines("[git]\ntemplate = \"{{ 1 }}\"");
        let err = lines
            .template_tree("git", &MjValue::from_serialize(json!({})))
            .unwrap_err();
        assert!(err.starts_with("git template: "), "{err}");
        let lines = self::lines("[git]\ntemplate = \"{{ {'a': 1} }}\"");
        let err = lines
            .template_tree("git", &MjValue::from_serialize(json!({})))
            .unwrap_err();
        assert!(err.contains("'a'"), "{err}");
    }

    fn returned(lines: &ProviderLines, provider: &str) -> Evaluated {
        lines
            .eval(
                provider,
                Key::Return,
                vec![
                    ("result", MjValue::from_serialize(json!({"a": 1}))),
                    ("template", MjValue::from_serialize(json!({"t": "x"}))),
                ],
            )
            .unwrap()
    }

    #[test]
    fn the_abstract_return_attaches_the_template() {
        let got = returned(&lines(""), "demo");
        assert_eq!(
            got.facets,
            vec![("template".to_string(), json!({"t": "x"}))]
        );
        assert_eq!(
            serde_json::to_value(got.value.unwrap()).unwrap(),
            json!({"a": 1}),
            "the data is handed on"
        );
    }

    #[test]
    fn a_return_without_facet_attaches_nothing() {
        let got = returned(&lines("[demo]\nreturn = \"{{ result }}\""), "demo");
        assert!(got.facets.is_empty());
    }

    /// `macro` is a facet, so a `return` may name it; only the `template`
    /// facet is read off a `return`, so it is ignored: a library is seeded
    /// from configuration, never handed out by a provider.
    #[test]
    fn a_macro_facet_on_a_return_is_accepted_and_ignored() {
        let lines = lines("[demo]\nreturn = \"{{ result | facet('macro', {'m': 'x'}) }}\"");
        let got = lines.returned("demo", None, || None, &|_, _| Ok(None));
        assert!(got.template.is_none(), "{:?}", got.template);
    }

    #[test]
    fn super_pipes_the_value_into_the_line_above() {
        let lines =
            lines("[demo]\nreturn = \"{{ result | facet('template', {'own': 'x'}) | super }}\"");
        let got = returned(&lines, "demo");
        assert_eq!(
            got.facets,
            vec![
                ("template".to_string(), json!({"own": "x"})),
                ("template".to_string(), json!({"t": "x"})),
            ],
            "the abstract's runs after"
        );
        assert_eq!(
            serde_json::to_value(got.value.unwrap()).unwrap(),
            json!({"a": 1})
        );
    }

    /// The facets are a closed set: a name outside it is a load error where
    /// the line writes it, and a render error where it is computed.
    #[test]
    fn facet_takes_only_a_facet_name() {
        for line in [
            "{{ result | facet('mine', 1) }}",
            "{{ result | facet( \\\"mine\\\", 1) | facet('template', template) }}",
        ] {
            let errs = errors(&format!("[demo]\nreturn = \"{line}\""));
            assert_eq!(
                errs,
                vec![
                    "[daemon.providers.demo] return: facet(): 'mine' is not a facet (a facet is \
                     template, macro, memo, put, source or cache)"
                        .to_string()
                ],
                "{line}"
            );
        }
        let lines = lines("[demo]\nreturn = \"{{ result | facet(_.name, 1) }}\"");
        let err = lines
            .eval(
                "demo",
                Key::Return,
                vec![("result", MjValue::from_serialize(json!({"a": 1})))],
            )
            .unwrap_err();
        assert!(
            err.contains(
                "facet(): 'demo' is not a facet (a facet is template, macro, memo, put, source or cache)"
            ),
            "{err}"
        );
    }

    #[test]
    fn super_with_nothing_above_is_a_render_error() {
        let lines = lines("[_]\nreturn = \"{{ result | super }}\"");
        let err = lines
            .eval("demo", Key::Return, vec![("result", MjValue::from(1))])
            .unwrap_err();
        assert!(err.contains("nothing above"), "{err}");
    }

    #[test]
    fn a_schedule_decorator_on_a_provider_line_is_a_load_error() {
        for (key, line) in [
            ("return", "{{ result | poll('5s') }}"),
            (
                "return",
                "{{ result | watch('a') | facet('template', template) }}",
            ),
            ("result", "{{ merge(sources) | ttl(5) }}"),
            ("return", "{{ result | nearest }}"),
        ] {
            let errs = errors(&format!("[demo]\n{key} = \"{line}\""));
            assert!(
                errs.len() == 1
                    && errs[0].starts_with(&format!("[daemon.providers.demo] {key}: "))
                    && errs[0].contains("schedule decorator"),
                "{line}: {errs:?}"
            );
        }
    }

    #[test]
    fn the_abstract_providers_lines_load_too() {
        let errs = errors("[_]\nreturn = \"{{ result | failback('1s', 3) }}\"");
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(
            errs[0].starts_with("[daemon.providers._] return: "),
            "{errs:?}"
        );
    }

    #[test]
    fn a_line_may_only_read_what_its_key_sees() {
        let errs = errors("[demo]\nresult = \"{{ client.x }}\"");
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("unknown name 'client'"), "{errs:?}");
        let errs = errors("[demo]\ntemplate = \"{{ sources.a() }}\"");
        assert!(errs[0].contains("unknown name 'sources'"), "{errs:?}");
        let errs = errors("[demo]\nreturn = \"{{ {{ \"");
        assert_eq!(errs.len(), 1, "a line that does not compile: {errs:?}");
        assert!(
            errs[0].starts_with("[daemon.providers.demo] return: "),
            "{errs:?}"
        );
    }

    #[test]
    fn a_render_failure_is_logged_once_per_message() {
        let lines = lines("[demo]\nresult = \"{{ 1 // 0 }}\"");
        let err = lines
            .eval("demo", Key::Result, vec![("sources", sources(&[]))])
            .unwrap_err();
        assert!(err.starts_with("demo result: "), "{err}");
        assert!(lines.note_error(&err), "the first is logged");
        assert!(!lines.note_error(&err), "and no later one");
        assert!(
            lines.note_error("demo result: a different failure"),
            "a later, different failure of the same line is logged too"
        );
    }

    #[test]
    fn the_default_result_is_told_by_its_text() {
        assert!(is_default_result("{{ merge(sources) }}"));
        assert!(is_default_result("{{merge( sources )}}"));
        assert!(!is_default_result("{{ merge(sources.a(), sources.b()) }}"));
    }

    #[test]
    fn super_on_a_template_or_result_line_is_a_load_error() {
        for (key, line) in [
            ("result", "{{ {'extra': 1} | super }}"),
            ("template", "{{ {'extra': 'x'} | super }}"),
        ] {
            let errs = errors(&format!("[demo]\n{key} = \"{line}\""));
            assert_eq!(errs.len(), 1, "{line}: {errs:?}");
            assert!(
                errs[0].starts_with(&format!(
                    "[daemon.providers.demo] {key}: | super means nothing"
                )) && errs[0].contains("write the whole line"),
                "{errs:?}"
            );
        }
        assert!(
            errors("[demo]\nreturn = \"{{ result | super }}\"").is_empty(),
            "on a return it pipes into the line above"
        );
    }

    #[test]
    fn a_template_or_result_line_cannot_read_a_piped_in_result() {
        let errs = errors("[demo]\ntemplate = \"{{ result }}\"");
        assert!(errs[0].contains("unknown name 'result'"), "{errs:?}");
        let errs = errors("[demo]\nresult = \"{{ result }}\"");
        assert!(errs[0].contains("unknown name 'result'"), "{errs:?}");
    }

    #[test]
    fn a_provider_line_cannot_call_anything_that_does_io() {
        for line in [
            "{{ exec('echo') }}",
            "{{ 'a' | exec }}",
            "{{ 'x' | read }}",
            "{{ 'x' | stat }}",
            "{{ 'x' | realpath }}",
            "{{ 'x*' | glob }}",
            "{{ lup('a') }}",
            "{{ url('http://x') }}",
            "{{ plugin('a', 'b') }}",
        ] {
            for key in ["template", "result", "return"] {
                let errs = errors(&format!("[demo]\n{key} = \"{line}\""));
                assert_eq!(errs.len(), 1, "{key} {line}: {errs:?}");
                assert!(
                    errs[0].contains("is not available in a provider's line"),
                    "{key} {line}: {errs:?}"
                );
            }
        }
        assert!(
            errors("[demo]\nreturn = \"{{ 'read' }}\"").is_empty(),
            "a word in a string is no call"
        );
    }

    /// `cache()` is a client template's: a provider's lines run on every
    /// read, and have no leaf to memoise. (`watch(...)` and `ttl(...)` there
    /// are the misplaced decorators.)
    #[test]
    fn a_provider_line_cannot_cache() {
        for key in ["template", "result", "return"] {
            assert_eq!(
                errors(&format!("[demo]\n{key} = \"{{{{ 'x' | cache() }}}}\"")),
                [format!(
                    "[daemon.providers.demo] {key}: cache is not available in a provider's \
                     line: these lines run on every read and do no IO"
                )],
                "{key}"
            );
        }
    }

    /// A provider's lines may define and call a macro within one line, and may
    /// not import a library: macros are client templates.
    #[test]
    fn a_provider_line_cannot_import() {
        for key in ["template", "result", "return"] {
            let errs = errors(&format!(
                "[demo]\n{key} = \"{{% from 'p:macro.x' import m %}}{{{{ m() }}}}\""
            ));
            assert_eq!(
                errs,
                [format!(
                    "[daemon.providers.demo] {key}: macros are client templates: a provider's \
                     line cannot import"
                )],
                "{key}"
            );
        }
        assert_eq!(
            errors("[demo]\nresult = \"{% macro two() %}2{% endmacro %}{{ two() }}\""),
            Vec::<String>::new(),
            "an inline macro loads"
        );
    }

    /// A `return` of statements and one `{{ }}` gives a map, not its text: a
    /// provider line may set names before what it answers.
    #[test]
    fn a_return_of_statements_is_a_map() {
        let lines = lines(
            "[demo]\nreturn = \"{% set extra = {'x': result.a + 1} %}{{ result | merge(extra) }}\"",
        );
        let got = returned(&lines, "demo");
        assert_eq!(
            serde_json::to_value(got.value.unwrap()).unwrap(),
            json!({"a": 1, "x": 2})
        );
    }

    /// The refusal is a reading of the library's tier table, not a list of its
    /// own: every item the table does not make available on a provider's line
    /// is refused by name, in whichever form it is called.
    #[test]
    fn a_provider_line_cannot_read_the_clock() {
        for line in [
            "{{ now() }}",
            "{{ 1 | strftime('%F') }}",
            "{{ 'x' | fromiso }}",
            "{{ 1 | ago }}",
            "{{ spinner('ab') }}",
        ] {
            for key in ["template", "result", "return"] {
                let errs = errors(&format!("[demo]\n{key} = \"{line}\""));
                assert_eq!(errs.len(), 1, "{key} {line}: {errs:?}");
                assert!(
                    errs[0].contains("is not available in a provider's line"),
                    "{key} {line}: {errs:?}"
                );
            }
        }
    }

    #[test]
    fn the_refusal_is_the_tier_table() {
        use libbeachcomber::library::{ITEMS, Surface};
        let mut refused = 0;
        for item in ITEMS {
            let available = item.tier.available(Surface::Provider);
            let call = match item.form {
                libbeachcomber::library::Form::Filter => format!("{{{{ 'x' | {} }}}}", item.name),
                _ => format!("{{{{ {}('x') }}}}", item.name),
            };
            let errs = errors(&format!("[demo]\nresult = \"{call}\""));
            let named = errs.iter().any(|e| {
                e.contains(&format!(
                    "{} is not available in a provider's line",
                    item.name
                ))
            });
            assert_eq!(named, !available, "{}: {errs:?}", item.name);
            refused += usize::from(!available);
        }
        assert!(refused >= 5, "exec, read, lup, url and plugin at least");
    }

    #[test]
    fn names_no_table_owns_share_the_abstract_providers_chains() {
        let lines = lines("[git]\nreturn = \"{{ result }}\"");
        assert!(Arc::ptr_eq(
            &lines.chains_for("nosuch1"),
            &lines.chains_for("nosuch2")
        ));
        assert!(!Arc::ptr_eq(
            &lines.chains_for("git"),
            &lines.chains_for("nosuch1")
        ));
        assert_eq!(
            lines.chains_for("nosuch1").effective(Key::Return),
            Some("{{ result | facet('template', template) }}")
        );
    }

    #[test]
    fn the_merge_filter_skips_none_like_the_function() {
        let lines = lines("[demo]\nresult = \"{{ sources.a() | merge(sources.b()) }}\"");
        let got = result(
            &lines,
            "demo",
            sources(&[("a", Some(json!({"x": "a"}))), ("b", None)]),
        );
        assert_eq!(got, json!({"x": "a"}), "a Source with nothing adds nothing");
    }

    #[test]
    fn the_return_line_kinds_are_told_apart() {
        let kind = |text: &str| lines(text).chains_for("demo").return_kind();
        assert_eq!(
            kind(""),
            ReturnKind::IdentityWithTemplate,
            "the shipped line"
        );
        assert_eq!(
            kind("[demo]\nreturn = \"{{result|facet(\\\"template\\\", template)}}\""),
            ReturnKind::IdentityWithTemplate,
            "whitespace and quotes aside"
        );
        assert_eq!(
            kind("[demo]\nreturn = \"{{ result }}\""),
            ReturnKind::Identity
        );
        assert_eq!(
            kind("[demo]\nreturn = \"{{ result | merge({'x': 1}) }}\""),
            ReturnKind::Line
        );
        assert_eq!(
            ProviderLines::new(&toml::Table::new())
                .unwrap()
                .chains_for("demo")
                .return_kind(),
            ReturnKind::Identity,
            "no line at all"
        );
        assert!(lines("").chains_for("demo").reads_by_default());
        assert!(
            !lines("[demo]\nresult = \"{{ {} }}\"")
                .chains_for("demo")
                .reads_by_default()
        );
        assert!(
            !lines("[demo]\nreturn = \"{{ result | merge({'x': 1}) }}\"")
                .chains_for("demo")
                .reads_by_default()
        );
    }

    fn fresh(source: &str, secs_ago: u64) -> RecordFreshness {
        RecordFreshness {
            source: Arc::from(source),
            entry: None,
            last_refreshed: Instant::now() - std::time::Duration::from_secs(secs_ago),
            expected_interval_secs: Some(60),
        }
    }

    fn read_of(pairs: &[(&str, i64)], from: &RecordFreshness) -> MergedRead {
        MergedRead {
            fields: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), Value::Int(*v)))
                .collect(),
            records: pairs
                .iter()
                .map(|(k, _)| (k.to_string(), from.clone()))
                .collect(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_returned_line_hands_out_its_value_with_honest_freshness() {
        let lines = lines(
            "[demo]\nreturn = \"{{ result | merge({'x': 1, 'a': 5}) | facet('template', template) }}\"",
        );
        let result = read_of(&[("a", 5), ("b", 6)], &fresh("one", 30));
        let tmpl = HashMap::from([("t".to_string(), Value::String("{{ 1 }}".into()))]);
        let got = lines.returned("demo", Some(result), || Some(tmpl.clone()), &|_, _| {
            Ok(None)
        });
        let read = got.read.expect("a value");
        assert_eq!(read.fields["x"], Value::Int(1));
        assert_eq!(read.fields["b"], Value::Int(6));
        assert_eq!(got.template, Some(tmpl));
        assert_eq!(&*read.records["a"].source, "one", "handed on as it was");
        assert_eq!(&*read.records["b"].source, "one");
        assert_eq!(
            &*read.records["x"].source, "computed:return",
            "a leaf the line made has no Source behind it"
        );
        assert!(crate::cache::is_static_label(&read.records["x"].source));
        assert_eq!(
            read.records["x"].last_refreshed, read.records["a"].last_refreshed,
            "as old as what it was computed from"
        );
    }

    #[test]
    fn the_identity_returns_hand_the_data_on_untouched() {
        let lines = lines("");
        let here = fresh("one", 0);
        let got = lines.returned(
            "demo",
            Some(read_of(&[("a", 1)], &here)),
            || None,
            &|_, _| Ok(None),
        );
        assert_eq!(got.read.unwrap().fields["a"], Value::Int(1));
        assert!(got.template.is_none(), "no facet to attach");
        let got = lines.returned(
            "demo",
            None,
            || Some(HashMap::from([("t".to_string(), Value::Int(1))])),
            &|_, _| Ok(None),
        );
        assert!(got.read.is_none());
        assert!(
            got.template.is_some(),
            "a template answers where the data does not"
        );
        let got = self::lines("[demo]\nreturn = \"{{ result }}\"").returned(
            "demo",
            None,
            || panic!("a return that never reads the template does not ask for it"),
            &|_, _| Ok(None),
        );
        assert!(got.template.is_none());
    }

    #[test]
    fn a_return_that_fails_or_gives_no_map_is_a_miss_logged_once() {
        let here = fresh("one", 0);
        let lines = lines("[demo]\nreturn = \"{{ 1 // 0 }}\"");
        let got = lines.returned(
            "demo",
            Some(read_of(&[("a", 1)], &here)),
            || None,
            &|_, _| Ok(None),
        );
        assert!(got.read.is_none(), "no silent fallback to the data");
        assert!(got.template.is_none());
        let message = lines.eval("demo", Key::Return, vec![]).unwrap_err();
        assert!(!lines.note_error(&message), "already logged by the first");

        let lines = self::lines("[demo]\nreturn = \"{{ 5 }}\"");
        let got = lines.returned(
            "demo",
            Some(read_of(&[("a", 1)], &here)),
            || None,
            &|_, _| Ok(None),
        );
        assert!(got.read.is_none(), "a return that is not a map is a miss");
        assert!(
            !lines.note_error("demo return: a provider's return is a map of fields"),
            "and was logged"
        );
    }

    #[test]
    fn a_full_error_log_says_so_once() {
        let lines = lines("");
        for i in 0..MAX_LOGGED {
            assert!(lines.note_error(&format!("failure {i}")));
        }
        assert!(!lines.note_error("one more"), "not logged as itself");
        assert!(
            lines.suppressed_notice_given(),
            "but the suppression was noted"
        );
    }

    #[test]
    fn a_dict_literal_does_not_end_a_tag_early() {
        let errs = errors("[demo]\nresult = \"{{ {'a': {'b': 1}} | merge(exec('x')) }}\"");
        assert!(errs[0].contains("is not available"), "{errs:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn what_a_line_computes_from_a_called_record_is_as_old_as_it() {
        let lines =
            lines("[demo]\nreturn = \"{{ result | merge(daemon.providers.q.sources.t()) }}\"");
        let old = fresh("t", 100);
        let record = CalledRecord {
            value: json!({"now": 5}),
            fresh: old.clone(),
        };
        let got = lines.returned(
            "demo",
            Some(read_of(&[("a", 1)], &fresh("one", 0))),
            || None,
            &|p, s| {
                assert_eq!((p, s), ("q", "t"));
                Ok(Some(record.clone()))
            },
        );
        let read = got.read.expect("a value");
        assert_eq!(read.fields["now"], Value::Int(5));
        assert_eq!(
            read.records["now"].last_refreshed, old.last_refreshed,
            "a leaf from a called record is as old as the record"
        );
    }

    #[test]
    fn a_call_that_fails_fails_the_line_only_if_the_line_makes_it() {
        let lines = self::lines(
            "[demo]\nreturn = \"{{ {'x': 1} if true else daemon.providers.q.sources.t() }}\"",
        );
        let got = lines.returned("demo", None, || None, &|_, _| Err("boom".into()));
        assert_eq!(got.read.expect("a value").fields["x"], Value::Int(1));
        let lines = self::lines("[demo]\nreturn = \"{{ daemon.providers.q.sources.t() }}\"");
        let got = lines.returned("demo", None, || None, &|_, _| Err("boom".into()));
        assert!(got.read.is_none(), "a call the line makes fails it");
    }

    #[test]
    fn a_disabled_provider_reads_the_empty_result() {
        let off = lines("[demo]\nenabled = false");
        let chains = off.chains_for("demo");
        assert!(chains.is_disabled());
        assert!(!chains.result_is_default());
        assert_eq!(
            result(&off, "demo", sources(&[("s", Some(json!({"a": 1})))])),
            json!({}),
            "its Sources' records are not read"
        );
        assert!(
            !lines("[demo]\nenabled = true")
                .chains_for("demo")
                .is_disabled()
        );
        assert!(!lines("[demo]").chains_for("demo").is_disabled());
        assert!(!lines("").chains_for("other").is_disabled());
    }

    #[test]
    fn disabled_wins_over_a_result_of_the_same_provider() {
        let off = lines("[demo]\nenabled = false\nresult = \"{{ {'a': 1} }}\"");
        assert!(off.chains_for("demo").is_disabled());
        assert_eq!(result(&off, "demo", sources(&[])), json!({}));
        // The result is still checked, whether or not it is read.
        let errs = errors("[demo]\nenabled = false\nresult = \"{{ nosuch }}\"");
        assert!(
            errs.iter().any(|e| e.contains("unknown name 'nosuch'")),
            "{errs:?}"
        );
    }

    #[test]
    fn a_disabled_provider_returns_the_data_with_no_template() {
        let off = lines("[demo]\nenabled = false");
        let chains = off.chains_for("demo");
        assert_eq!(chains.return_kind(), ReturnKind::Identity);
        let got = returned(&off, "demo");
        assert!(got.facets.is_empty(), "{:?}", got.facets);
        // A `return` of its own is not read either.
        let own = lines("[demo]\nenabled = false\nreturn = \"{{ {'x': 1} }}\"");
        assert_eq!(own.chains_for("demo").return_kind(), ReturnKind::Identity);
    }

    #[test]
    fn a_disabled_provider_merges_nothing() {
        for (text, default) in [
            ("[demo]\nenabled = false", false),
            (
                "[demo]\nenabled = false\nresult = \"{{ merge(sources) }}\"",
                false,
            ),
            ("[demo]\nenabled = true", true),
            ("[demo]", true),
            ("[demo]\nresult = \"{{ {} }}\"", false),
        ] {
            let lines = ProviderLines::new(&over_shipped(text)).unwrap();
            assert_eq!(
                lines.chains_for("demo").result_is_default(),
                default,
                "{text}"
            );
        }
    }

    #[test]
    fn sources_is_not_a_name_a_return_sees() {
        let errs = errors("[demo]\nreturn = \"{{ merge(sources) }}\"");
        assert!(
            errs.len() == 1 && errs[0].contains("unknown name 'sources'"),
            "{errs:?}"
        );
    }

    #[test]
    fn the_abstract_provider_cannot_call_a_source_by_name() {
        for key in ["result", "return"] {
            let errs = errors(&format!("[_]\n{key} = \"{{{{ sources.head() }}}}\""));
            assert!(
                errs.iter().any(|e| e.starts_with(&format!(
                    "[daemon.providers._] {key}: sources.head(): the abstract provider applies to \
                     every provider"
                )) && e.ends_with("use merge(sources)")),
                "{key}: {errs:?}"
            );
        }
        assert!(errors("[_]\nresult = \"{{ merge(sources) }}\"").is_empty());
    }
}
