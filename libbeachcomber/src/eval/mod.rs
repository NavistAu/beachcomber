//! One expression syntax: classification, reference discovery, rendering.
//!
//! Canon `field_resolution.md` §"Value resolution" (invariant 14): `{{ }}`
//! everywhere. A value expression written as exactly one `{{ expr }}` evaluates
//! to the expression's natural type; one written with literal text or more than
//! one tag evaluates to a string. A source with no tags at all is its literal
//! text — there is no bare-expression form.
//!
//! This module answers the two questions a caller has before evaluating — which
//! of the two forms is this ([`classify`], [`single_tag_expression`]), and what
//! does it reference ([`discover_refs`]) — and owns the template render
//! ([`render_template`]). Evaluation itself is [`crate::resolve`]'s: it
//! fetches what a source references from the daemon and evaluates it against
//! that, through this module's primitives.

mod scan;

pub use scan::{Tag, TagKind, scan_tags};

use crate::virtual_fields::{Ref, build_expression_env, discover_expression_refs, refs_from_names};
use minijinja::value::Value as MjValue;

// ── Forms ─────────────────────────────────────────────────────────────────────

/// How a value expression is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// Exactly one `{{ }}` tag spanning the whole source — keeps its natural type.
    SingleTag,
    /// Literal text, more than one tag, any non-expression tag, an
    /// unterminated tag marker, no tags at all, or an empty source —
    /// string-valued, and the template compiler owns any syntax diagnostic.
    ///
    /// A source with no tags at all (empty, whitespace-only, or plain text)
    /// is a template of no tags: it renders to its own literal text (`""` for
    /// an empty source). There is no bare-expression form — `-f ''` and
    /// `comb eval hello` are both legitimate things to ask for.
    Template,
}

// ── Classification ────────────────────────────────────────────────────────────

/// Classify a value expression into one of the two [`Form`]s.
///
/// Surrounding whitespace is not literal text: it is trimmed before the tag
/// spans are compared against the source.
///
/// `{{` always opens a tag, in every form — so literal text containing `{{`
/// (`"a {{ b"`) is read as an unterminated tag and classified
/// [`Form::Template`]. That is the cost of one unambiguous rule for where a
/// tag starts; write such a literal inside a tag (`{{ "a {{ b" }}`) — the
/// scanner tracks string literals within a tag, so it stays one tag.
pub fn classify(src: &str) -> Form {
    match analyze(src) {
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
        Analysis::Template => None,
    }
}

/// The form of `src`, carrying the inner expression for the single-tag form.
///
/// [`Form`] is the public shape of this; `Analysis` is the internal one, so
/// every consumer matches exhaustively on the two cases and none has to
/// handle a "single tag with no expression" that cannot occur.
///
/// One scan, one place the single-tag rule is written down, so [`classify`],
/// [`single_tag_expression`] and [`discover_refs`] can never disagree about
/// what the single-tag form is.
enum Analysis<'a> {
    /// Exactly one `{{ }}` tag spanning the whole source, with its trimmed body.
    SingleTag(&'a str),
    /// Everything else: no tags at all, literal text, more than one tag, an
    /// unterminated tag marker, or an empty source.
    Template,
}

fn analyze(src: &str) -> Analysis<'_> {
    let trimmed = src.trim_ascii();
    // Nothing to compile; a template of no tags renders to its own text
    // ("" when empty).
    if trimmed.is_empty() {
        return Analysis::Template;
    }
    let scan = scan::scan(trimmed);
    if scan.unterminated {
        return Analysis::Template;
    }
    match scan.tags.as_slice() {
        [tag] if tag.kind == TagKind::Expression && tag.start == 0 && tag.end == trimmed.len() => {
            Analysis::SingleTag(tag.body)
        }
        _ => Analysis::Template,
    }
}

// ── Reference discovery ───────────────────────────────────────────────────────

/// Every reference in `src`, for both forms, deduplicated and sorted.
///
/// A `SingleTag` source goes through MiniJinja's expression meta-analysis; a
/// `Template` source through the template's. Both run on the same environment
/// and classify the dotted names they find identically
/// (`crate::virtual_fields::classify_dotted`): a `P.rest…` name is
/// `Resolved(P, "rest…")` with every segment kept, so `scanline.widgets.b`
/// names that leaf and never the group above it. A source that fails to
/// compile yields no refs — the compile error surfaces at evaluation time. A
/// tagless source is literal text, so it references nothing.
///
/// Both analyses hand back a `HashSet`, whose iteration order varies run to
/// run, so **every discovery path in this crate returns its refs sorted** —
/// this function and the `discover_expression_refs` it delegates to. That is
/// for reproducibility alone: nothing downstream depends on the order.
/// [`crate::resolve`] in particular binds a `cache.P` / `cache.P.F` collision
/// the same way whichever it binds first.
pub fn discover_refs(src: &str) -> Vec<Ref> {
    match analyze(src) {
        // Already sorted by `discover_expression_refs`.
        Analysis::SingleTag(expr) => discover_expression_refs(expr),
        Analysis::Template => {
            // Ref discovery never evaluates anything, so `allow_exec`
            // doesn't matter here — `false` is an arbitrary, harmless
            // choice. (This has nothing to do with `exec` showing up as an
            // "undeclared variable": minijinja's meta-analysis never
            // reports a call's callee name at all, registered or not.)
            let env = build_expression_env(false);
            let Ok(template) = env.template_from_str(src) else {
                return Vec::new();
            };
            let mut refs = refs_from_names(template.undeclared_variables(true));
            refs.sort();
            refs
        }
    }
}

/// How deeply one template may reference another before resolution gives up.
///
/// The resolver's frame stack (`Resolver::push` in [`crate::resolve`]) is
/// cycle-guarded, but a cycle guard only stops a *repeat*: a chain of N
/// distinct nodes, each referencing the next, visits every one exactly once
/// and recurses N frames deep. Nothing bounds N — a `put` or a config file can
/// declare thousands — and blowing the native stack in a cdylib is not a
/// recoverable error for the host process, it is a SIGSEGV in someone else's
/// program. So `push` refuses a frame past this depth instead.
///
/// 128 is far past any legible cascade and far short of the stack.
pub(crate) const MAX_VIRTUAL_DEPTH: usize = 128;

/// The message the resolver returns at [`MAX_VIRTUAL_DEPTH`].
pub(crate) fn too_deep() -> String {
    format!("virtual field nesting too deep (limit {MAX_VIRTUAL_DEPTH})")
}

// ── Rendering ─────────────────────────────────────────────────────────────────

/// Render `src` as a template against an already-assembled `context`.
///
/// The formatting surfaces' one template render: `-f fmt`, `watch -f fmt` and
/// the status formatter's custom templates all land here, with `exec()`
/// unavailable. [`crate::resolve`] renders the template form of a value
/// expression through the same `render_template_with`, with `exec()` as the
/// surface decided.
///
/// `context` is a JSON object whose keys are the template's top-level names:
/// the shape a caller rendering pre-fetched provider data builds directly.
///
/// The environment is the typed path's — the same filters, the same lenient
/// undefined behaviour — so a cascade over a missing ref reads the same in both
/// forms. On top of that, a `none` (a JSON `null`: a ref that resolved to
/// nothing) renders as the empty string rather than the literal word `none`,
/// matching how a missing `env.*` and [`crate::render::render_data`] already
/// render nothing.
pub fn render_template(src: &str, context: &serde_json::Value) -> Result<String, String> {
    render_template_with(src, context, false).map_err(|e| e.to_string())
}

/// Why a template render failed, by phase: the source itself does not
/// compile, or it compiled and failed while rendering. A caller that has to
/// tell a configuration fault from a runtime one ([`crate::resolve`]) matches
/// the variant; the public `Result<String, String>` render above carries the
/// same split as the two prefixed strings `Display` produces.
#[derive(Debug)]
pub(crate) enum RenderError {
    Compile(minijinja::Error),
    Render(minijinja::Error),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderError::Compile(e) => write!(f, "template compile error: {e}"),
            RenderError::Render(e) => write!(f, "template render error: {e}"),
        }
    }
}

pub(crate) fn render_template_with(
    src: &str,
    context: &serde_json::Value,
    allow_exec: bool,
) -> Result<String, RenderError> {
    let mut env = build_expression_env(allow_exec);
    env.set_formatter(|out, state, value| {
        if value.is_none() {
            Ok(())
        } else {
            minijinja::escape_formatter(out, state, value)
        }
    });
    let template = env.template_from_str(src).map_err(RenderError::Compile)?;
    template
        .render(MjValue::from_serialize(context))
        .map_err(RenderError::Render)
}
