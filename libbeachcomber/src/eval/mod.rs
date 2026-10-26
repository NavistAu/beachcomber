//! One expression syntax: classification, reference discovery, evaluation.
//!
//! Canon `field_resolution.md` §"Value resolution" (invariant 14): `{{ }}`
//! everywhere. A value expression written as exactly one `{{ expr }}` evaluates
//! to the expression's natural type; one written with literal text or more than
//! one tag evaluates to a string. A source with no tags at all is its literal
//! text — there is no bare-expression form.
//!
//! This module answers the questions a caller has before evaluating — which of
//! the forms is this ([`classify`], [`single_tag_expression`]), what does it
//! reference ([`discover_refs`]) and import ([`imports`]), and is it sound
//! where it is written ([`lint`]) — and owns the one evaluator, [`evaluate`],
//! that the client's templates ([`crate::resolve`], which fetches what a
//! source references first), a Source's lines and a provider's lines all run.

mod lint;
mod scan;

pub use lint::{Finding, Known, Line, lint, refused};
pub use scan::{Tag, TagKind, scan_tags};

use crate::engine::build_engine_env;
use crate::virtual_fields::{Ref, discover_expression_refs, refs_from_names};
use minijinja::Environment;
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

// ── Imports ───────────────────────────────────────────────────────────────────

/// What one `{% import … as … %}` or `{% from … import … %}` names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Import<'a> {
    /// A string literal: the library's address, as the engine reads it.
    Named(String),
    /// Anything else: a name the text would compute when it renders. The
    /// tag's body, as written.
    Computed(&'a str),
}

/// Every import in `src`, in source order: the name each `{% import %}` and
/// `{% from %}` reads its library from. A tag in a `{% raw %}` block is text,
/// not an import.
pub fn imports(src: &str) -> Vec<Import<'_>> {
    scan::engine_tags(src)
        .into_iter()
        .filter(|tag| tag.kind == TagKind::Statement)
        .filter_map(|tag| {
            let (keyword, rest) = scan::statement(tag.body);
            let follows = match keyword {
                "import" => "as",
                "from" => "import",
                _ => return None,
            };
            Some(match string_literal(rest) {
                Some((name, after)) if scan::statement(after).0 == follows => Import::Named(name),
                _ => Import::Computed(tag.body),
            })
        })
        .collect()
}

/// The string literal `text` starts with, as the engine reads it, and the text
/// after it; `None` when `text` does not start with one.
fn string_literal(text: &str) -> Option<(String, &str)> {
    let quote = *text
        .as_bytes()
        .first()
        .filter(|q| matches!(q, b'"' | b'\''))?;
    let bytes = text.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b if b == quote => {
                let literal = &text[..=i];
                // The engine's own reading of the literal, escapes and all.
                let value = Environment::empty()
                    .compile_expression(literal)
                    .and_then(|e| e.eval(()))
                    .ok()?;
                return Some((
                    value.as_str()?.to_string(),
                    text[i + 1..].trim_ascii_start(),
                ));
            }
            _ => i += 1,
        }
    }
    None
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
            let mut refs = refs_from_names(not_globals(&env, template.undeclared_variables(true)));
            refs.sort();
            refs
        }
    }
}

/// `names` without the engine's own globals: a function called by name
/// (`exec(...)`, `lup(...)`) is reported as a name read, and it is not one the
/// context supplies.
pub(crate) fn not_globals(
    env: &Environment<'_>,
    names: impl IntoIterator<Item = String>,
) -> impl Iterator<Item = String> {
    let globals: std::collections::HashSet<String> =
        env.globals().map(|(name, _)| name.to_string()).collect();
    names
        .into_iter()
        .filter(move |name| !globals.contains(name))
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

// ── Evaluation ────────────────────────────────────────────────────────────────

/// Why a text failed to evaluate, by phase, and in which [`Form`] it is
/// written: it does not compile, or holds a statement no text may
/// ([`refused`]), and someone has to go and edit it; or it compiled and
/// failed while it ran, which is the moment's. The message is the error's own,
/// without the engine's frame ([`without_engine_frame`]) and never with the
/// context. `Display` prefixes it with its form and phase
/// (`template compile error: …`), as the client reports it; a daemon line
/// reports the [`message`](EvalError::message) alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvalError {
    Compile { form: Form, message: String },
    Runtime { form: Form, message: String },
}

impl EvalError {
    fn compile(form: Form, e: &minijinja::Error) -> Self {
        EvalError::Compile {
            form,
            message: without_engine_frame(&e.to_string()),
        }
    }

    fn runtime(form: Form, e: &minijinja::Error) -> Self {
        EvalError::Runtime {
            form,
            message: without_engine_frame(&e.to_string()),
        }
    }

    /// What went wrong, without the form and phase.
    pub fn message(&self) -> &str {
        match self {
            EvalError::Compile { message, .. } | EvalError::Runtime { message, .. } => message,
        }
    }
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self {
            EvalError::Compile {
                form: Form::SingleTag,
                ..
            } => "expression compile error",
            EvalError::Runtime {
                form: Form::SingleTag,
                ..
            } => "expression eval error",
            EvalError::Compile {
                form: Form::Template,
                ..
            } => "template compile error",
            EvalError::Runtime {
                form: Form::Template,
                ..
            } => "template render error",
        };
        write!(f, "{what}: {}", self.message())
    }
}

/// Evaluate `src` in `env` against `context`, the one evaluator every line
/// runs through: exactly one `{{ }}` keeps its expression's type; anything
/// else, literal text included, renders to a string, a `none` printed in it
/// as nothing. A `none` or undefined value is `Ok(None)`: absent.
///
/// A text a load checked (a seeded template, a Source's or provider's line)
/// passed [`lint`] there; one nothing checked (`comb eval`, `-f`) is refused
/// here the same way, structurally, as a compile error ([`refused`]).
pub fn evaluate(
    env: &Environment<'_>,
    src: &str,
    context: MjValue,
) -> Result<Option<MjValue>, EvalError> {
    let value = match analyze(src) {
        Analysis::SingleTag(expr) => {
            let form = Form::SingleTag;
            let compiled = env
                .compile_expression(expr)
                .map_err(|e| EvalError::compile(form, &e))?;
            compiled
                .eval(context)
                .map_err(|e| EvalError::runtime(form, &e))?
        }
        Analysis::Template => {
            let form = Form::Template;
            if let Some(finding) = refused(src, Line::Template).into_iter().next() {
                return Err(EvalError::Compile {
                    form,
                    message: finding.to_string(),
                });
            }
            let template = env
                .template_from_str(src)
                .map_err(|e| EvalError::compile(form, &e))?;
            MjValue::from(
                template
                    .render(context)
                    .map_err(|e| EvalError::runtime(form, &e))?,
            )
        }
    };
    Ok((!(value.is_undefined() || value.is_none())).then_some(value))
}

/// An engine error's message without the engine's own framing: the leading
/// `invalid operation: ` and a trailing ` (in <expression>:1)` or
/// ` (in <string>:1)`. Only those two places, once: the text between is the
/// error's own, a command's stderr included, and is not touched.
pub fn without_engine_frame(message: &str) -> String {
    let body = message
        .strip_prefix("invalid operation: ")
        .unwrap_or(message);
    if let Some(open) = body.rfind(" (in <")
        && let Some(rest) = body[open..].strip_suffix(')')
        && let Some((place, line)) = rest[" (in <".len()..].split_once(">:")
        && !place.is_empty()
        && place.chars().all(|c| c.is_ascii_lowercase() || c == '-')
        && !line.is_empty()
        && line.chars().all(|c| c.is_ascii_digit())
    {
        return body[..open].to_string();
    }
    body.to_string()
}

/// [`evaluate`] one daemon line against `context` (a JSON object of the line's
/// top-level names), its value converted the way the client's templates
/// convert one. Errors are the [`EvalError::message`]; they never include the
/// context.
pub fn eval_line(
    env: &Environment<'_>,
    src: &str,
    context: &serde_json::Value,
) -> Result<Option<serde_json::Value>, String> {
    eval_line_with(env, src, MjValue::from_serialize(context))
}

/// [`eval_line`] against a context that is already an engine value, so it
/// may hold objects — a daemon line's lazy `comb`.
pub fn eval_line_with(
    env: &Environment<'_>,
    src: &str,
    context: MjValue,
) -> Result<Option<serde_json::Value>, String> {
    match evaluate(env, src, context).map_err(|e| e.message().to_string())? {
        Some(value) => crate::virtual_fields::mj_to_json(value),
        None => Ok(None),
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
// Tests for a template rendered over locals — what a `-f` template sees of
// the value it formats, through `resolve::eval_with` like every template —
// and for reference discovery.
#[cfg(test)]
mod tests {
    use super::discover_refs;
    use crate::virtual_fields::Ref;

    /// `src` rendered over `ctx`'s fields as locals, with no daemon behind
    /// it and `exec()` off: what a `-f` template sees of the value it formats.
    fn render_template(src: &str, ctx: &serde_json::Value) -> Result<String, String> {
        use crate::resolve::{Fetch, FetchError, Got, Inputs, Slot, eval_with};
        struct NoDaemon;
        impl Fetch for NoDaemon {
            fn get(
                &mut self,
                _: &str,
                _: Option<&str>,
                _: bool,
                _: bool,
            ) -> Result<Option<Got>, FetchError> {
                Ok(None)
            }
        }
        let env = std::collections::HashMap::new();
        let inputs = Inputs {
            cwd: "",
            env: &env,
            slot: Slot::Global,
            force: false,
            wait: false,
            allow_exec: false,
            now: std::time::SystemTime::now(),
        };
        let locals = ctx.as_object().cloned().unwrap_or_default();
        eval_with(src, "<test>", &inputs, &mut NoDaemon, &locals)
            .map(|v| crate::render::render_value("", v.as_ref(), crate::render::Format::Text))
            .map_err(|e| e.to_string())
    }

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
        assert_eq!(out, "abcdef…");
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
        let tpl = "{{ git.branch | fg('#00ff00') }}";
        // With no terminal in the render's environment the colour is 16-colour.
        let out = render_template(tpl, &ctx).unwrap();
        assert_eq!(out, "\x1b[92mmain\x1b[0m");
        let truecolor = crate::engine::EngineCtx {
            env: [("COLORTERM".to_string(), "truecolor".to_string())].into(),
            ..Default::default()
        };
        let env = crate::engine::build_engine_env(std::sync::Arc::new(truecolor));
        let out = super::evaluate(&env, tpl, minijinja::Value::from_serialize(&ctx))
            .unwrap()
            .unwrap();
        assert_eq!(out.as_str(), Some("\x1b[38;2;0;255;0mmain\x1b[0m"));
    }
}

#[cfg(test)]
mod line_tests {
    use super::{EvalError, Form, eval_line, evaluate, undeclared_names, without_engine_frame};
    use crate::engine::build_engine_env;
    use minijinja::value::Value as MjValue;
    use serde_json::json;

    fn run(src: &str, context: serde_json::Value) -> Result<Option<serde_json::Value>, String> {
        eval_line(&build_engine_env(Default::default()), src, &context)
    }

    /// `src` through the one evaluator, over `context`.
    fn evaluated(src: &str, context: serde_json::Value) -> Result<Option<MjValue>, EvalError> {
        evaluate(
            &build_engine_env(Default::default()),
            src,
            MjValue::from_serialize(context),
        )
    }

    /// One `{{ }}` keeps its type; anything else is text, `none` in it
    /// nothing; `none` and undefined are absent.
    #[test]
    fn evaluate_keeps_one_tags_type_and_renders_the_rest_as_text() {
        let value = |src: &str| {
            evaluated(src, json!({"d": {"a": [1, "x"]}}))
                .unwrap()
                .map(|v| serde_json::to_value(v).unwrap())
        };
        assert_eq!(value("{{ 1 }}"), Some(json!(1)));
        assert_eq!(value("  {{- d -}}\n"), Some(json!({"a": [1, "x"]})));
        assert_eq!(value("x{{ 1 }}"), Some(json!("x1")));
        assert_eq!(value("{{ 1 }}{{ 2 }}"), Some(json!("12")));
        assert_eq!(value("a{{ none }}b"), Some(json!("ab")));
        assert_eq!(value("lit"), Some(json!("lit")));
        assert_eq!(value(""), Some(json!("")));
        assert_eq!(value("{{ none }}"), None);
        assert_eq!(value("{{ d.nope.x }}"), None);
    }

    /// Not compiling is a compile error; failing while it runs is a runtime
    /// one: each in its form's words, the engine's frame taken off.
    #[test]
    fn evaluate_tells_a_compile_error_from_a_runtime_one() {
        let cases = [
            ("{{ 1 + }}", "expression compile error: "),
            ("{{ 'a' + 1 }}", "expression eval error: "),
            ("a {{ 1 + }}", "template compile error: "),
            ("a {{ 'a' + 1 }}", "template render error: "),
        ];
        for (src, words) in cases {
            let err = evaluated(src, json!({})).unwrap_err();
            let shown = err.to_string();
            assert!(shown.starts_with(words), "{src}: {shown}");
            assert_eq!(shown, format!("{words}{}", err.message()), "{src}");
            assert!(!err.message().contains("(in <"), "{src}: {shown}");
        }
        assert!(matches!(
            evaluated("{{ 1 + }}", json!({})),
            Err(EvalError::Compile {
                form: Form::SingleTag,
                ..
            })
        ));
        assert!(matches!(
            evaluated("a {{ 'a' + 1 }}", json!({})),
            Err(EvalError::Runtime {
                form: Form::Template,
                ..
            })
        ));
    }

    /// A text nothing linted is refused as a linted one would be, before it
    /// renders: `include`, `extends` and `block`, and an import not named by a
    /// string.
    #[test]
    fn evaluate_refuses_an_unlinted_text_structurally() {
        for (src, why) in [
            (
                "{% include 'p:macro.x' %}",
                "{% include %} is refused: text is reused with a macro",
            ),
            (
                "{% block b %}x{% endblock %}",
                "{% block %} is refused: text is reused with a macro",
            ),
            (
                "{% import name as m %}{{ m.x() }}",
                "{% import name as m %}: an import names its library as a string, \
                 \"<p>:macro.<name>\"",
            ),
        ] {
            assert_eq!(
                evaluated(src, json!({})),
                Err(EvalError::Compile {
                    form: Form::Template,
                    message: why.into()
                }),
                "{src}"
            );
        }
    }

    /// The engine's frame is taken off once, at its two places, and the text
    /// between is the error's own.
    #[test]
    fn only_the_engines_own_frame_is_taken_off_an_error() {
        assert_eq!(
            without_engine_frame("invalid operation: boom (in <expression>:1)"),
            "boom"
        );
        assert_eq!(
            without_engine_frame("invalid operation: boom (in <template>:12)"),
            "boom"
        );
        for kept in [
            "sh said invalid operation: nope (in the middle) and went on",
            "a command wrote (in <expression>:1) in its stderr and then more",
            "plain",
        ] {
            assert_eq!(without_engine_frame(kept), kept);
        }
        assert_eq!(
            without_engine_frame("invalid operation: x: invalid operation: y (in <expression>:1)"),
            "x: invalid operation: y",
            "once: the text between is the error's own"
        );
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
