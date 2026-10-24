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

use crate::engine::{EngineCtx, build_engine_env};
use crate::virtual_fields::{Ref, discover_expression_refs, refs_from_names};
use minijinja::Environment;
use minijinja::value::Value as MjValue;
use std::sync::Arc;

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
/// [`crate::resolve`] in particular binds a `comb.P` / `comb.P.F` collision
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
            let env = build_engine_env(Default::default());
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
    render_template_with(src, context, Default::default()).map_err(|e| e.to_string())
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
    ctx: Arc<EngineCtx>,
) -> Result<String, RenderError> {
    let mut env = build_engine_env(ctx);
    render_none_as_empty(&mut env);
    let template = env.template_from_str(src).map_err(RenderError::Compile)?;
    template
        .render(MjValue::from_serialize(context))
        .map_err(RenderError::Render)
}

/// Make a `none` printed inside text render as nothing instead of the word
/// `none`. The one place that rule is installed.
pub fn render_none_as_empty(env: &mut Environment<'_>) {
    env.set_formatter(|out, state, value| {
        if value.is_none() {
            Ok(())
        } else {
            minijinja::escape_formatter(out, state, value)
        }
    });
}

/// Evaluate one line against `context` (a JSON object of the line's
/// top-level names) in `env`: exactly one `{{ }}` keeps its expression's type,
/// converted the way the client's templates convert it; any other mix is a
/// string with `none` printed as nothing; text with no tags is literal. A
/// `none` or undefined result is `Ok(None)`: absent. The caller installs
/// [`render_none_as_empty`] when it builds `env`. Errors are messages; they
/// never include the context.
pub fn eval_line(
    env: &Environment<'_>,
    src: &str,
    context: &serde_json::Value,
) -> Result<Option<serde_json::Value>, String> {
    let context = MjValue::from_serialize(context);
    match single_tag_expression(src) {
        Some(expr) => {
            let value = env
                .compile_expression(expr)
                .and_then(|compiled| compiled.eval(context))
                .map_err(|e| e.to_string())?;
            crate::virtual_fields::mj_to_json(value)
        }
        None => env
            .render_str(src, context)
            .map(|s| Some(serde_json::Value::String(s)))
            .map_err(|e| e.to_string()),
    }
}

/// The variables `src` reads that nothing in it declares: the names a context
/// must supply. Functions and filters are not variables. A source that does
/// not compile is an `Err`.
///
/// Not `nested`, each is a top-level name (`comb`). `nested`, each is the
/// path read (`comb.aws.region`), and a bare name means the variable is used
/// other than by attribute (`comb['aws']`, `comb | length`).
pub fn undeclared_names(
    env: &Environment<'_>,
    src: &str,
    nested: bool,
) -> Result<std::collections::BTreeSet<String>, String> {
    let names = match single_tag_expression(src) {
        Some(expr) => env
            .compile_expression(expr)
            .map_err(|e| e.to_string())?
            .undeclared_variables(nested),
        None => env
            .template_from_str(src)
            .map_err(|e| e.to_string())?
            .undeclared_variables(nested),
    };
    // A registered function called in a template (`exec(...)`) is reported
    // too: it is not a name the context supplies.
    let globals: std::collections::HashSet<&str> = env.globals().map(|(name, _)| name).collect();
    Ok(names
        .into_iter()
        .filter(|name| !globals.contains(name.as_str()))
        .collect())
}

// ── Tests ─────────────────────────────────────────────────────────────────────
//
// Tests for the CLI's `-f fmt` rendering and for the value-expression
// rendering it now shares with `comb eval` and the `:template` facet.
//
// Task 3 deleted `src/cli/format.rs`'s hand-rolled `{{ }}` scanner
// (`find_eval_template_pairs` / `find_eval_template_refs`) and its
// `render_eval_template`: reference discovery is `eval::discover_refs` and
// rendering is `eval::render_template`, both here. The scanner cases that had
// no library equivalent are kept below, restated against `discover_refs`.
//
// Plan 4 Task 1 deleted `render_fmt_template`, the `ProviderResult`-keyed
// wrapper around `render_fmt_template_json` that nothing in `src/` called any
// more; its `fmt_template_*` cases went with it, each already restated below
// as an `eval_template_*` case exercising the same `render_template`.
#[cfg(test)]
mod tests {
    use super::{discover_refs, render_template};
    use crate::virtual_fields::Ref;

    fn resolved(provider: &str, field: &str) -> Ref {
        Ref::Resolved(provider.into(), field.into())
    }

    // --- eval template tests ---

    #[test]
    fn eval_template_renders_provider_field_refs() {
        let ctx = serde_json::json!({
            "hostname": {"value": "me-laptop"},
            "load": {"one": 0.42, "five": 0.3, "fifteen": 0.25}
        });
        let out =
            render_template("hostname: {{ hostname.value }}, load: {{ load.one }}", &ctx).unwrap();
        assert_eq!(out, "hostname: me-laptop, load: 0.42");
    }

    #[test]
    fn eval_template_truncate_filter_works_on_fields() {
        let ctx = serde_json::json!({
            "git": {"sha": "abcdef1234567890"}
        });
        let out = render_template("{{ git.sha | truncate(7) }}", &ctx).unwrap();
        assert_eq!(out, "abcdef1...");
    }

    #[test]
    fn eval_template_conditional_works() {
        let ctx = serde_json::json!({
            "git": {"dirty": true, "branch": "main"}
        });
        let out = render_template("{% if git.dirty %}*{% endif %}{{ git.branch }}", &ctx).unwrap();
        assert_eq!(out, "*main");
    }

    #[test]
    fn eval_template_default_filter_for_missing_field() {
        let ctx = serde_json::json!({"git": {"branch": "main"}});
        let out = render_template("{{ git.tag | default('no-tag') }}", &ctx).unwrap();
        assert_eq!(out, "no-tag");
    }

    #[test]
    fn eval_template_single_brace_is_literal() {
        let ctx = serde_json::json!({"git": {"branch": "main"}});
        let out = render_template("{git.branch}", &ctx).unwrap();
        assert_eq!(out, "{git.branch}");
    }

    // --- reference discovery (was the deleted `find_eval_template_pairs`) ---

    #[test]
    fn discover_refs_finds_refs_in_for_block() {
        assert_eq!(
            discover_refs("{% for x in items.list %}{{ x }}{% endfor %}"),
            vec![resolved("items", "list")]
        );
    }

    #[test]
    fn discover_refs_skips_comment_blocks() {
        assert_eq!(
            discover_refs("{# git.dirty #}{{ hostname.name }}"),
            vec![resolved("hostname", "name")]
        );
    }

    #[test]
    fn discover_refs_handles_whitespace_control_dashes() {
        assert_eq!(
            discover_refs("{%- if git.dirty -%}*{%- endif -%}"),
            vec![resolved("git", "dirty")]
        );
    }

    #[test]
    fn discover_refs_combines_block_and_expression_refs() {
        assert_eq!(
            discover_refs(
                "{% if git.dirty %}dirty:{{ git.branch }}{% else %}{{ git.branch }}{% endif %}"
            ),
            vec![resolved("git", "branch"), resolved("git", "dirty")]
        );
    }

    #[test]
    fn discover_refs_skips_dotted_string_literals() {
        // A dotted string literal is not a ref.
        assert_eq!(
            discover_refs(r#"{{ git.branch or "foo.bar" }}"#),
            vec![resolved("git", "branch")]
        );
    }

    #[test]
    fn eval_template_accepts_tab_and_newline_whitespace_inside_braces() {
        let ctx = serde_json::json!({"git": {"branch": "main"}});
        let out = render_template("{{\tgit.branch }}", &ctx).unwrap();
        assert_eq!(out, "main");

        assert_eq!(
            discover_refs("{{\tgit.branch }}"),
            vec![resolved("git", "branch")]
        );
    }

    #[test]
    fn render_template_fg_filter_wraps_in_truecolor_sgr() {
        let ctx = serde_json::json!({"git": {"branch": "main"}});
        let out = render_template("{{ git.branch | fg('#00ff00') }}", &ctx).unwrap();
        assert_eq!(out, "\x1b[38;2;0;255;0mmain\x1b[0m");
    }
}

#[cfg(test)]
mod line_tests {
    use super::{eval_line, render_none_as_empty, undeclared_names};
    use crate::engine::build_engine_env;
    use serde_json::json;

    fn run(src: &str, context: serde_json::Value) -> Result<Option<serde_json::Value>, String> {
        {
            let mut env = build_engine_env(Default::default());
            render_none_as_empty(&mut env);
            eval_line(&env, src, &context)
        }
    }

    #[test]
    fn one_tag_keeps_the_clients_type() {
        assert_eq!(run("{{ 1 }}", json!({})).unwrap(), Some(json!(1)));
        assert_eq!(run("{{ true }}", json!({})).unwrap(), Some(json!(true)));
        assert_eq!(
            run("{{ 4 / 2 }}", json!({})).unwrap(),
            Some(json!(2)),
            "the number type the client's mj_to_json produces"
        );
        assert_eq!(
            run("{{ d }}", json!({"d": {"a": [1, "x"]}})).unwrap(),
            Some(json!({"a": [1, "x"]}))
        );
    }

    #[test]
    fn a_number_json_cannot_hold_is_an_error() {
        let err = run("{{ 1 / 0 }}", json!({})).unwrap_err();
        assert!(err.contains("inf"), "unexpected message: {err}");
    }

    #[test]
    fn a_mix_is_a_string_and_no_tags_is_literal() {
        assert_eq!(run("x{{ 1 }}", json!({})).unwrap(), Some(json!("x1")));
        assert_eq!(run("{{ 1 }}{{ 2 }}", json!({})).unwrap(), Some(json!("12")));
        assert_eq!(run("lit", json!({})).unwrap(), Some(json!("lit")));
    }

    #[test]
    fn none_and_undefined_are_absent_and_none_in_text_is_nothing() {
        assert_eq!(run("{{ none }}", json!({})).unwrap(), None);
        assert_eq!(run("{{ nope.x }}", json!({})).unwrap(), None);
        assert_eq!(run("a{{ none }}b", json!({})).unwrap(), Some(json!("ab")));
    }

    #[test]
    fn errors_are_strings() {
        assert!(run("{{ 'a' + 1 }}", json!({})).is_err());
        assert!(run("{{ ", json!({})).is_err());
    }

    #[test]
    fn undeclared_names_are_the_top_level_variables() {
        let env = build_engine_env(Default::default());
        let names = |src: &str| undeclared_names(&env, src, false).unwrap();
        assert_eq!(
            names("{{ comb.a.b ~ env.X }}")
                .into_iter()
                .collect::<Vec<_>>(),
            vec!["comb", "env"]
        );
        assert_eq!(
            names("a {{ data | trim }} {{ exec('x') }}")
                .into_iter()
                .collect::<Vec<_>>(),
            vec!["data"]
        );
        assert!(undeclared_names(&env, "{{ ", false).is_err());
        let nested = |src: &str| undeclared_names(&env, src, true).unwrap();
        assert!(
            nested("{{ comb.a.b }}")
                .iter()
                .all(|n| n.starts_with("comb."))
        );
        assert!(nested("{{ comb['a'].b }}").contains("comb"));
        assert!(nested("{{ comb | length }}").contains("comb"));
        assert!(!nested("{{ comb.a['b'] }}").contains("comb"));
    }
}
