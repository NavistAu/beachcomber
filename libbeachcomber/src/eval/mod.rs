//! One expression syntax: classification, reference discovery, evaluation.
//!
//! Canon `field_resolution.md` §"Value resolution" (invariant 14): `{{ }}`
//! everywhere. A value expression written as exactly one `{{ expr }}` evaluates
//! to the expression's natural type; one written with literal text or more than
//! one tag evaluates to a string. A bare expression (no tags) is still accepted
//! and is equivalent to the single-tag form.
//!
//! This module answers the two questions a caller has before evaluating — which
//! of the three forms is this ([`classify`]), and what does it reference
//! ([`discover_refs`], [`daemon_refs`], [`fetch_daemon_data`]) — and then
//! evaluates it ([`evaluate`]). [`render_template`] is the workspace's one
//! template render, for callers that have already assembled a context.

mod scan;

pub use scan::{Tag, TagKind, scan_tags};

use crate::virtual_fields::{
    EvalContext, Ref, VirtualFields, build_context_json, build_expression_env,
    discover_expression_refs, mj_to_json, refs_from_names,
};
use minijinja::value::Value as MjValue;
use std::collections::{HashMap, HashSet};

// ── Forms ─────────────────────────────────────────────────────────────────────

/// How a value expression is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// No tag markers at all — the whole source is the expression (backward
    /// compatible).
    Expression,
    /// Exactly one `{{ }}` tag spanning the whole source — keeps its natural type.
    SingleTag,
    /// Literal text, more than one tag, any non-expression tag, an
    /// unterminated tag marker, or an empty source — string-valued, and the
    /// template compiler owns any syntax diagnostic.
    ///
    /// An empty (or whitespace-only) source is a template of no tags: it
    /// renders to `""`. Reading it as an expression instead would make it a
    /// compile error, and `-f ''` is a legitimate thing to ask for.
    Template,
}

// ── Classification ────────────────────────────────────────────────────────────

/// Classify a value expression into one of the three [`Form`]s.
///
/// Surrounding whitespace is not literal text: it is trimmed before the tag
/// spans are compared against the source.
///
/// `{{` always opens a tag, in every form — so a bare expression whose *string
/// literal* contains `{{` (`"a {{ b"`) is read as an unterminated tag and
/// classified [`Form::Template`], not as the expression it was before one
/// syntax. That is the cost of one unambiguous rule for where a tag starts;
/// write such a literal inside a tag (`{{ "a {{ b" }}`) — the scanner tracks
/// string literals within a tag, so it stays one tag.
pub fn classify(src: &str) -> Form {
    match analyze(src) {
        Analysis::Expression => Form::Expression,
        Analysis::SingleTag(_) => Form::SingleTag,
        Analysis::Template => Form::Template,
    }
}

/// The expression inside the tag, when `src` is the [`Form::SingleTag`] form.
///
/// Whitespace-control markers and surrounding whitespace are trimmed, so
/// `{{- git.branch -}}` yields `git.branch`.
pub fn single_tag_expression(src: &str) -> Option<&str> {
    match analyze(src) {
        Analysis::SingleTag(expr) => Some(expr),
        Analysis::Expression | Analysis::Template => None,
    }
}

/// The form of `src`, carrying the inner expression for the single-tag form.
///
/// [`Form`] is the public shape of this; `Analysis` is the internal one, so
/// every consumer matches exhaustively on the three cases and none has to
/// handle a "single tag with no expression" that cannot occur.
///
/// One scan, one place the single-tag rule is written down, so [`classify`],
/// [`single_tag_expression`], [`discover_refs`] and [`evaluate`] can never
/// disagree about what the single-tag form is.
enum Analysis<'a> {
    /// No tag markers at all — the whole (untrimmed) source is the expression.
    Expression,
    /// Exactly one `{{ }}` tag spanning the whole source, with its trimmed body.
    SingleTag(&'a str),
    /// Everything else, an empty source included.
    Template,
}

fn analyze(src: &str) -> Analysis<'_> {
    let trimmed = src.trim_ascii();
    // Nothing to compile; a template of no tags renders to "".
    if trimmed.is_empty() {
        return Analysis::Template;
    }
    let scan = scan::scan(trimmed);
    if scan.unterminated {
        return Analysis::Template;
    }
    match scan.tags.as_slice() {
        [] => Analysis::Expression,
        [tag] if tag.kind == TagKind::Expression && tag.start == 0 && tag.end == trimmed.len() => {
            Analysis::SingleTag(tag.body)
        }
        _ => Analysis::Template,
    }
}

// ── Reference discovery ───────────────────────────────────────────────────────

/// Every reference in `src`, for all three forms, deduplicated and sorted.
///
/// `Expression` and `SingleTag` sources go through MiniJinja's expression
/// meta-analysis; `Template` sources through the template's. Both run on the
/// same environment and classify the dotted names they find identically. A
/// source that fails to compile yields no refs — the compile error surfaces at
/// evaluation time.
///
/// Both analyses hand back a `HashSet`, whose iteration order varies run to
/// run, so **every discovery path in this crate returns its refs sorted** —
/// this function and the `discover_expression_refs` it delegates to. That is
/// for reproducibility alone: nothing downstream depends on the order.
/// `build_context_json` in particular resolves a `cache.P` / `cache.P.F`
/// collision the same way whichever it binds first.
pub fn discover_refs(src: &str) -> Vec<Ref> {
    match analyze(src) {
        // Already sorted by `discover_expression_refs`.
        Analysis::Expression => discover_expression_refs(src),
        Analysis::SingleTag(expr) => discover_expression_refs(expr),
        Analysis::Template => {
            let env = build_expression_env();
            let Ok(template) = env.template_from_str(src) else {
                return Vec::new();
            };
            let mut refs = refs_from_names(template.undeclared_variables(true));
            refs.sort();
            refs
        }
    }
}

/// How deeply one virtual field may reference another before evaluation gives
/// up. Both recursions over the virtual-field graph — the ref closure
/// ([`close_over_virtuals`]) and the evaluation itself
/// ([`crate::virtual_fields::build_context_json`] ↔
/// [`crate::virtual_fields::VirtualFields::evaluate`]) — are cycle-guarded, but
/// a cycle guard only stops a *repeat*: a chain of N distinct fields, each
/// referencing the next, visits every one exactly once and recurses N frames
/// deep. Nothing bounds N — a `put`, an `overrides_json` or a config file can
/// declare thousands — and blowing the native stack in a cdylib is not a
/// recoverable error for the host process, it is a SIGSEGV in someone else's
/// program.
///
/// 128 is far past any legible cascade and far short of the stack.
pub(crate) const MAX_VIRTUAL_DEPTH: usize = 128;

/// The message both recursions return at [`MAX_VIRTUAL_DEPTH`].
pub(crate) fn too_deep() -> String {
    format!("virtual field nesting too deep (limit {MAX_VIRTUAL_DEPTH})")
}

/// The refs a caller must fetch from the daemon before evaluating `src`.
///
/// The transitive closure of [`discover_refs`] over virtual fields: every
/// `Resolved(p, f)` that is itself virtual is replaced by the refs of its own
/// expression, recursively. The result holds only `CacheField`, `CacheProvider`
/// and non-virtual `Resolved` refs — deduplicated and sorted. `Env` refs come
/// from the caller's shell, never the daemon, and are dropped.
///
/// A reference cycle terminates: each virtual field is expanded at most once.
/// A chain longer than [`MAX_VIRTUAL_DEPTH`] is an `Err` rather than a deeper
/// recursion.
pub fn daemon_refs(src: &str, vf: &VirtualFields) -> Result<Vec<Ref>, String> {
    let mut out = Vec::new();
    let mut seen: HashSet<Ref> = HashSet::new();
    let mut expanded: HashSet<(String, String)> = HashSet::new();
    close_over_virtuals(
        &discover_refs(src),
        vf,
        &mut out,
        &mut seen,
        &mut expanded,
        0,
    )?;
    // Expansion interleaves each virtual field's own refs into the walk, so a
    // sorted input does not stay sorted through the closure.
    out.sort();
    Ok(out)
}

fn close_over_virtuals(
    refs: &[Ref],
    vf: &VirtualFields,
    out: &mut Vec<Ref>,
    seen: &mut HashSet<Ref>,
    expanded: &mut HashSet<(String, String)>,
    depth: usize,
) -> Result<(), String> {
    if depth >= MAX_VIRTUAL_DEPTH {
        return Err(too_deep());
    }
    for r in refs {
        match r {
            // env.* is read from the calling shell, not fetched.
            Ref::Env(_) => {}
            Ref::Resolved(p, f) if vf.is_virtual(p, f) => {
                // `expanded` doubles as the cycle guard: a field already
                // expanded (on this path or another) is never expanded again.
                if expanded.insert((p.clone(), f.clone()))
                    && let Some(expr) = vf.expression(p, f)
                {
                    close_over_virtuals(&discover_refs(expr), vf, out, seen, expanded, depth + 1)?;
                }
            }
            other => {
                if seen.insert(other.clone()) {
                    out.push(other.clone());
                }
            }
        }
    }
    Ok(())
}

/// Fetch each ref's value through `fetch`, keyed the way
/// [`EvalContext::daemon_data`](crate::virtual_fields::EvalContext) expects.
///
/// `CacheField(p, f)` and `Resolved(p, f)` are asked for as `"p.f"`;
/// `CacheProvider(p)` as `"p"`. `Env` refs are skipped. A `Ok(None)` is a miss
/// and is simply absent from the map.
///
/// `fetch` is called at most once per distinct key: `cache.p.f` and a resolved
/// `p.f` are different refs but the same daemon query, and a miss is not worth
/// a second round trip either.
///
/// The first `Err` short-circuits, so the caller decides what a transport
/// failure means — abort the render, or fall back to what did arrive. The
/// closure shape lets a `Client`, a `Session`, or a foreign source supply the
/// values.
pub fn fetch_daemon_data<E>(
    refs: &[Ref],
    mut fetch: impl FnMut(&str) -> Result<Option<serde_json::Value>, E>,
) -> Result<HashMap<String, serde_json::Value>, E> {
    let mut data = HashMap::new();
    let mut asked: HashSet<String> = HashSet::new();
    for r in refs {
        let key = match r {
            Ref::Env(_) => continue,
            Ref::CacheField(p, f) | Ref::Resolved(p, f) => format!("{p}.{f}"),
            Ref::CacheProvider(p) => p.clone(),
        };
        if !asked.insert(key.clone()) {
            continue;
        }
        if let Some(v) = fetch(&key)? {
            data.insert(key, v);
        }
    }
    Ok(data)
}

// ── Evaluation ────────────────────────────────────────────────────────────────
//
// ERROR-MESSAGE ABI CONTRACT. The two compile failures below are prefixed
// `"expression compile error: "` (the typed path, `evaluate_expression`) and
// `"template compile error: "` (the render path, `render_template`). Those
// prefixes are not cosmetic: `libbeachcomber-ffi::eval_error_kind` matches the
// substring `"compile error"` in them to report `parse_error` — the caller's
// own source being malformed — and everything else as `server_error`. The
// runtime failures deliberately do NOT carry it (`"expression eval error: "`,
// `"template render error: "`), and neither does `VirtualFields::evaluate`'s
// cycle error. Reword either prefix and every SDK silently reclassifies its
// syntax errors as server faults, so change the two together with
// `eval_error_kind` and the FFI tests that pin both halves
// (`bc_eval_compile_error_is_parse_error_kind`,
// `bc_eval_runtime_error_is_server_error_kind`). `docs/roadmap.md` carries the
// proper fix: a typed `EvalError` the FFI can match on instead.

/// Evaluate a value expression in any of the three [`Form`]s.
///
/// `Expression` and `SingleTag` take the typed path — the expression is
/// compiled and evaluated, and the result keeps its natural type. `Template`
/// renders and yields a `Value::String`.
///
/// `ctx` must already hold every value [`daemon_refs`] asked for; a ref with no
/// value is falsy rather than an error.
pub fn evaluate(
    src: &str,
    vf: &VirtualFields,
    ctx: &EvalContext<'_>,
) -> Result<serde_json::Value, String> {
    evaluate_with_stack(src, vf, ctx, &mut HashSet::new())
}

/// [`evaluate`], threading the caller's virtual-field evaluation stack.
///
/// Cycle detection lives in [`VirtualFields::evaluate`], which owns the stack;
/// this is the entry point it and [`evaluate`] share, so a virtual field
/// written with tags is cycle-checked exactly like a bare one.
pub(crate) fn evaluate_with_stack(
    src: &str,
    vf: &VirtualFields,
    ctx: &EvalContext<'_>,
    stack: &mut HashSet<(String, String)>,
) -> Result<serde_json::Value, String> {
    match analyze(src) {
        Analysis::Expression => evaluate_expression(src, vf, ctx, stack),
        Analysis::SingleTag(expr) => evaluate_expression(expr, vf, ctx, stack),
        Analysis::Template => {
            let context = build_context_json(&discover_refs(src), ctx, vf, stack)?;
            render_template(src, &context).map(serde_json::Value::String)
        }
    }
}

/// The typed path: compile `expr` as an expression and keep its natural type.
fn evaluate_expression(
    expr: &str,
    vf: &VirtualFields,
    ctx: &EvalContext<'_>,
    stack: &mut HashSet<(String, String)>,
) -> Result<serde_json::Value, String> {
    let refs = discover_expression_refs(expr);
    let ctx_json = build_context_json(&refs, ctx, vf, stack)?;
    let top = MjValue::from_serialize(&ctx_json);

    let env = build_expression_env();
    let compiled = env
        .compile_expression(expr)
        .map_err(|e| format!("expression compile error: {e}"))?;
    let mj_result = compiled
        .eval(top)
        .map_err(|e| format!("expression eval error: {e}"))?;

    Ok(mj_to_json(mj_result))
}

/// Render `src` as a template against an already-assembled `context`.
///
/// The workspace's only template render: `comb eval`'s template form, `-f fmt`,
/// `watch -f fmt` and the status formatter's custom templates all land here.
///
/// `context` is a JSON object whose keys are the template's top-level names:
/// the shape the [`Form::Template`] path resolves an [`EvalContext`] to, and
/// the shape a caller rendering pre-fetched provider data builds directly.
///
/// The environment is the typed path's — the same filters, the same lenient
/// undefined behaviour — so a cascade over a missing ref reads the same in both
/// forms. On top of that, a `none` (a JSON `null`: a ref that resolved to
/// nothing) renders as the empty string rather than the literal word `none`,
/// matching how a missing `env.*` and [`crate::render::render_data`] already
/// render nothing.
pub fn render_template(src: &str, context: &serde_json::Value) -> Result<String, String> {
    let mut env = build_expression_env();
    env.set_formatter(|out, state, value| {
        if value.is_none() {
            Ok(())
        } else {
            minijinja::escape_formatter(out, state, value)
        }
    });
    let template = env
        .template_from_str(src)
        .map_err(|e| format!("template compile error: {e}"))?;
    template
        .render(MjValue::from_serialize(context))
        .map_err(|e| format!("template render error: {e}"))
}
