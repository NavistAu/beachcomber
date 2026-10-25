//! The environment a value expression evaluates in.
//!
//! Two things live here, shared by [`crate::eval`] (classification,
//! discovery, the template render) and [`crate::resolve`] (the one client-side
//! resolver): the reference model — [`Ref`], and the classifier that reads
//! one out of a dotted name minijinja's `undeclared_variables(true)`
//! meta-analysis reports; and the conversion from a minijinja result back to
//! JSON (`mj_to_json`). The environment itself is [`crate::engine`]'s.
//!
//! Nothing here holds a template. The daemon holds them, as data at
//! `provider:template`, and `crate::resolve` fetches and renders them; this
//! module neither knows which keys are templates nor evaluates one on its own.
//!
//! RUST NOTE: `virtual` is a reserved keyword in Rust, which is why this
//! module is `virtual_fields`.

use minijinja::value::Value as MjValue;
use serde_json::Value as JsonValue;
use std::collections::HashSet;

// ── Types ─────────────────────────────────────────────────────────────────────

/// A kinded reference discovered in an expression.
///
/// The reference kinds of canon `field_resolution.md`:
/// - `env.X` → `Env(X)`
/// - `comb.P.F` → `CombField(P, F)` (the raw cached value, never a template)
/// - `comb.P` → `CombProvider(P)` (the whole provider object)
/// - `P.rest…` (P ∉ {env, comb}) → `Resolved(P, "rest…")` (the node at that
///   address, resolved — rendered if a template stands there)
/// - a name alone — `comb[...]`, `git['x']`, a whole name piped to a filter —
///   → `Dynamic(name)`
///
/// `Ord` is derived: variants sort in declaration order (Env < CombField <
/// CombProvider < Resolved), then field-wise. Discovery runs off minijinja's
/// `HashSet` of undeclared variables, so sorting is what makes ref order
/// reproducible across runs — see `crate::eval::discover_refs`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ref {
    /// `env.X` — the calling shell's environment variable X.
    Env(String),
    /// `comb.P.F` — the raw cached field value, bypassing any template.
    CombField(String, String),
    /// `comb.P` — the whole provider object from the cache.
    CombProvider(String),
    /// `P.rest…` — the resolved node at that address. The second component is
    /// the whole remaining address (`"widgets.b"`), not one segment: a ref
    /// names a node at any depth, never the group above it.
    Resolved(String, String),
    /// A top-level name used other than by attribute — `comb[...]`,
    /// `git['x']`, a whole `env` piped to `items`, or the name alone. What it
    /// reads is known only as the render asks, so [`crate::resolve`] binds a
    /// lazy object there.
    Dynamic(String),
}

// ── Ref discovery ─────────────────────────────────────────────────────────────

/// Classify one dotted name from minijinja's undeclared-variable analysis.
///
/// - `env.X` → `Ref::Env(X)`
/// - `comb.P.F` → `Ref::CombField(P, F)`
/// - `comb.P` → `Ref::CombProvider(P)`
/// - `cwd` → `None` (path-expression variable)
/// - `P.rest…` (P ∉ {env, comb, cwd}) → `Ref::Resolved(P, "rest…")`
/// - a name alone → `Ref::Dynamic(name)`: minijinja reports a name alone when
///   it is used other than by attribute (`comb['x']`, `git[k]`, a filter)
///
/// For `env.*` and `comb.*`, segments beyond the second (third for
/// `comb.*`) are MiniJinja attribute navigation into the fetched value, not
/// part of the ref. For every other name the whole remainder is the ref:
/// `scanline.widgets.b` names that leaf, and [`crate::resolve`] fetches it
/// — never the group above it.
///
/// Shared by [`discover_expression_refs`] and the template discovery path in
/// [`crate::eval`], so expression and template forms classify identically.
pub(crate) fn classify_dotted(name: &str) -> Option<Ref> {
    let Some((first, rest)) = name.split_once('.') else {
        // A name alone is a name used other than by attribute.
        return (name != "cwd").then(|| Ref::Dynamic(name.to_string()));
    };
    let mut segments = rest.split('.');
    // `split` yields at least one item, so `second` is always present.
    let second = segments.next()?;
    let third = segments.next();

    match first {
        "env" => Some(Ref::Env(second.to_string())),
        "comb" => Some(match third {
            Some(field) => Ref::CombField(second.to_string(), field.to_string()),
            None => Ref::CombProvider(second.to_string()),
        }),
        // Path-expression variable, never a ref.
        "cwd" => None,
        _ => Some(Ref::Resolved(first.to_string(), rest.to_string())),
    }
}

/// Classify a batch of undeclared-variable names into a deduplicated ref list.
///
/// Shared by [`discover_expression_refs`] and the template discovery path in
/// `crate::eval`, so the expression and template forms of one value expression
/// always yield the same refs.
///
/// VERIFIED against vendor/minijinja 2.19.0 (compiler/meta.rs:141-157): with
/// nested = true, an attribute chain `a.b.c` is recorded as the dotted string
/// "a.b.c" (e.g. `env.PYENV_VERSION or mise.python` → {"env.PYENV_VERSION",
/// "mise.python"}). So the classifier splits each entry on '.' — no
/// byte-scanning needed.
pub(crate) fn refs_from_names(names: impl IntoIterator<Item = String>) -> Vec<Ref> {
    let mut refs: Vec<Ref> = Vec::new();
    let mut seen: HashSet<Ref> = HashSet::new();
    for name in names {
        if let Some(r) = classify_dotted(&name)
            && seen.insert(r.clone())
        {
            refs.push(r);
        }
    }
    refs
}

/// Discover all refs in an expression using minijinja's
/// `Expression::undeclared_variables(true)` (nested = true), classified by
/// [`classify_dotted`].
///
/// Returns a deduplicated list of refs, sorted for reproducibility — see
/// [`crate::eval::discover_refs`], which states the rule for every discovery
/// path in this crate.
///
/// Takes an expression, not a value expression: a source written with `{{ }}`
/// fails to compile here and yields nothing. [`crate::eval::discover_refs`] is
/// the entry point that handles both forms, and is what callers outside
/// this module use.
pub(crate) fn discover_expression_refs(expr: &str) -> Vec<Ref> {
    // Ref discovery never evaluates anything, so `allow_exec` doesn't
    // matter here — an empty ctx is an arbitrary, harmless choice. (This has
    // nothing to do with `exec` showing up as an "undeclared variable":
    // minijinja's meta-analysis never reports a call's callee name at all,
    // registered or not.)
    let env = crate::engine::build_engine_env(Default::default());
    let Ok(compiled) = env.compile_expression(expr) else {
        return vec![];
    };
    let mut refs = refs_from_names(crate::eval::not_globals(
        &env,
        compiled.undeclared_variables(true),
    ));
    refs.sort();
    refs
}

// ── Value conversion ──────────────────────────────────────────────────────────

/// Convert the value a one-tag line evaluated to into the value it yields:
/// the one rule the client's templates and the daemon's lines share.
///
/// Preserves types: bool → bool, integer → number, string → string, map →
/// object, sequence → array. `none` and undefined are absent (`None`): null
/// means absent, never `""`. A number JSON cannot hold (`1 / 0`) is an error.
pub(crate) fn mj_to_json(v: MjValue) -> Result<Option<JsonValue>, String> {
    if v.is_undefined() || v.is_none() {
        return Ok(None);
    }
    // A lazy ref (`comb['git:cache']`, a provider named alone) yields the
    // node it stands for, and nothing when nothing is there.
    if let Some(whole) = crate::lazy::as_whole(&v) {
        return Ok(whole);
    }
    // Try bool first (minijinja booleans are distinct from strings).
    if v.kind() == minijinja::value::ValueKind::Bool
        && let Ok(b) = v.clone().try_into() as Result<bool, _>
    {
        return Ok(Some(JsonValue::Bool(b)));
    }
    // Try integer.
    if v.kind() == minijinja::value::ValueKind::Number {
        if let Ok(n) = v.clone().try_into() as Result<i64, _> {
            return Ok(Some(JsonValue::Number(serde_json::Number::from(n))));
        }
        if let Ok(n) = v.clone().try_into() as Result<u64, _> {
            return Ok(Some(JsonValue::Number(serde_json::Number::from(n))));
        }
        if let Ok(f) = v.clone().try_into() as Result<f64, _> {
            return serde_json::Number::from_f64(f)
                .map(|n| Some(JsonValue::Number(n)))
                .ok_or_else(|| format!("{v} is not a number JSON can hold"));
        }
    }
    // Structured values keep their shape: an interior node is an object and a
    // sequence an array, rather than MiniJinja's `{"a": 1}` debug rendering —
    // canon `field_resolution.md` invariants 8 and 12. serde round-trips the
    // whole tree (a nested `none` becomes JSON null); anything it cannot
    // represent falls through to the string form below.
    if matches!(
        v.kind(),
        minijinja::value::ValueKind::Map
            | minijinja::value::ValueKind::Seq
            | minijinja::value::ValueKind::Iterable
    ) && let Ok(json) = serde_json::to_value(&v)
    {
        return Ok(Some(json));
    }
    // Strings, and anything else as its text.
    Ok(Some(JsonValue::String(v.to_string())))
}
