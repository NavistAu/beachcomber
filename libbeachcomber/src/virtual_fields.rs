//! The environment a value expression evaluates in.
//!
//! Three things live here, all shared by [`crate::eval`] (classification,
//! discovery, the template render) and [`crate::resolve`] (the one client-side
//! resolver): the reference model — [`Ref`], and the classifier that reads
//! one out of a dotted name minijinja's `undeclared_variables(true)`
//! meta-analysis reports; the minijinja [`Environment`] those expressions
//! compile against (`build_expression_env`: the shared filters, chainable
//! undefined, and the `exec()` function in its enabled or disabled form); and
//! the conversion from a minijinja result back to JSON (`mj_to_json`).
//!
//! Nothing here holds a template. The daemon holds them, as data at
//! `provider:template`, and `crate::resolve` fetches and renders them; this
//! module neither knows which keys are templates nor evaluates one on its own.
//!
//! RUST NOTE: `virtual` is a reserved keyword in Rust, which is why this
//! module is `virtual_fields`.

use minijinja::value::{Kwargs, Rest, Value as MjValue, ValueKind};
use minijinja::{Environment, Error, ErrorKind, UndefinedBehavior};
use serde_json::Value as JsonValue;
use std::collections::HashSet;

// ── Types ─────────────────────────────────────────────────────────────────────

/// A kinded reference discovered in an expression.
///
/// The reference kinds of canon `field_resolution.md`:
/// - `env.X` → `Env(X)`
/// - `cache.P.F` → `CacheField(P, F)` (the raw cached value, never a template)
/// - `cache.P` → `CacheProvider(P)` (the whole provider object)
/// - `P.rest…` (P ∉ {env, cache}) → `Resolved(P, "rest…")` (the node at that
///   address, resolved — rendered if a template stands there)
///
/// `Ord` is derived: variants sort in declaration order (Env < CacheField <
/// CacheProvider < Resolved), then field-wise. Discovery runs off minijinja's
/// `HashSet` of undeclared variables, so sorting is what makes ref order
/// reproducible across runs — see `crate::eval::discover_refs`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ref {
    /// `env.X` — the calling shell's environment variable X.
    Env(String),
    /// `cache.P.F` — the raw cached field value, bypassing any template.
    CacheField(String, String),
    /// `cache.P` — the whole provider object from the cache.
    CacheProvider(String),
    /// `P.rest…` — the resolved node at that address. The second component is
    /// the whole remaining address (`"widgets.b"`), not one segment: a ref
    /// names a node at any depth, never the group above it.
    Resolved(String, String),
}

// ── Ref discovery ─────────────────────────────────────────────────────────────

/// Classify one dotted name from minijinja's undeclared-variable analysis.
///
/// - `env.X` → `Ref::Env(X)`
/// - `cache.P.F` → `Ref::CacheField(P, F)`
/// - `cache.P` → `Ref::CacheProvider(P)`
/// - `cwd` → `None` (path-expression variable)
/// - `P.rest…` (P ∉ {env, cache, cwd}) → `Ref::Resolved(P, "rest…")`
/// - bare single name → `None`
///
/// For `env.*` and `cache.*`, segments beyond the second (third for
/// `cache.*`) are MiniJinja attribute navigation into the fetched value, not
/// part of the ref. For every other name the whole remainder is the ref:
/// `scanline.widgets.b` names that leaf, and [`crate::resolve`] fetches it
/// — never the group above it.
///
/// Shared by [`discover_expression_refs`] and the template discovery path in
/// [`crate::eval`], so expression and template forms classify identically.
pub(crate) fn classify_dotted(name: &str) -> Option<Ref> {
    // Bare name — no ref (includes "cwd").
    let (first, rest) = name.split_once('.')?;
    let mut segments = rest.split('.');
    // `split` yields at least one item, so `second` is always present.
    let second = segments.next()?;
    let third = segments.next();

    match first {
        "env" => Some(Ref::Env(second.to_string())),
        "cache" => Some(match third {
            Some(field) => Ref::CacheField(second.to_string(), field.to_string()),
            None => Ref::CacheProvider(second.to_string()),
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
    // matter here — `false` is an arbitrary, harmless choice. (This has
    // nothing to do with `exec` showing up as an "undeclared variable":
    // minijinja's meta-analysis never reports a call's callee name at all,
    // registered or not.)
    let env = build_expression_env(false);
    let Ok(compiled) = env.compile_expression(expr) else {
        return vec![];
    };
    let mut refs = refs_from_names(compiled.undeclared_variables(true));
    refs.sort();
    refs
}

// ── Minijinja environment for expressions ─────────────────────────────────────

/// Build a minijinja `Environment` suitable for `compile_expression`.
///
/// Registers the same filters as `build_env()`, sets
/// chainable undefined behavior so missing refs are falsy, not errors, and
/// registers the `exec()` jinja function: the real closure ([`exec_fn`])
/// when `allow_exec` is true, a closure that always errors ([`exec_disabled_fn`])
/// otherwise. Both modes register *some* function named `exec` so a
/// disabled call fails with the on-topic "exec() is disabled in this
/// context" rather than minijinja's generic "unknown function: exec" —
/// indistinguishable, to the person reading it, from a typo'd function
/// name.
///
/// `Chainable` rather than `Lenient` because canon `field_resolution.md` says a
/// missing ref is falsy, and that has to hold at any depth: `Lenient` renders
/// an undefined value as empty but *errors* on attribute access into one, so
/// `{{ p.f.sub }}` blew up whenever `p.f` missed while `{{ p.f }}` quietly
/// rendered nothing. Chaining through undefined yields undefined instead.
/// Cascades are unaffected — undefined is falsy under both — and `default`
/// still fires, since the resolver binds nothing for a missed ref.
pub(crate) fn build_expression_env<'a>(allow_exec: bool) -> Environment<'a> {
    use crate::filters::build_env;
    let mut env = build_env();
    env.set_undefined_behavior(UndefinedBehavior::Chainable);
    if allow_exec {
        env.add_function("exec", exec_fn);
    } else {
        env.add_function("exec", exec_disabled_fn);
    }
    env
}

// ── exec() ────────────────────────────────────────────────────────────────

/// The `exec()` stub registered when `allow_exec` is false: same name and
/// calling convention as [`exec_fn`], so a template that calls `exec(...)`
/// fails at the call site with a clear message rather than at compile time
/// with an "unknown function" error.
fn exec_disabled_fn(_args: Rest<MjValue>, _kwargs: Kwargs) -> Result<MjValue, Error> {
    Err(Error::new(
        ErrorKind::InvalidOperation,
        "exec() is disabled in this context",
    ))
}

/// The real `exec()` jinja function, registered only when `allow_exec` is
/// true: `exec(cmd, arg1, arg2, ..., timeout_ms=10000, stdin=none)`, backed
/// by the spawn engine in [`crate::exec`].
///
/// `cmd` (the first positional) must be a string. Every remaining positional
/// is coerced to a string argv entry — see [`coerce_exec_argv`]. `timeout_ms`
/// (default 10000) must convert to an integer; `stdin` (default none) must
/// be a string — `| tojson` first for structured data, symmetric with argv
/// coercion rejecting a sequence or map. Any other keyword argument is an
/// error.
fn exec_fn(args: Rest<MjValue>, kwargs: Kwargs) -> Result<MjValue, Error> {
    let cmd = args.first().ok_or_else(|| {
        Error::new(
            ErrorKind::MissingArgument,
            "exec() requires at least a command argument",
        )
    })?;
    let cmd = cmd.as_str().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidOperation,
            "exec(): argument 1 (the command) must be a string",
        )
    })?;

    let mut argv = Vec::with_capacity(args.len().saturating_sub(1));
    for (i, v) in args.iter().enumerate().skip(1) {
        argv.push(coerce_exec_argv(v, i + 1)?);
    }

    // A bare `?` here would surface a conversion failure (e.g.
    // `timeout_ms="x"`) as minijinja's generic "cannot convert string to
    // u64", with nothing naming `exec()` or which keyword caused it.
    let timeout_ms = kwargs
        .get::<Option<u64>>("timeout_ms")
        .map_err(|e| {
            Error::new(
                ErrorKind::InvalidOperation,
                format!(
                    "exec(): timeout_ms= {}",
                    e.detail().unwrap_or("invalid value")
                ),
            )
        })?
        .unwrap_or(10_000);
    // Fetched as a raw `Value` (not `Option<String>`) so a non-string —
    // e.g. `stdin=true` or `stdin={"a": 1}` — is rejected instead of
    // silently stringifying through minijinja's `Display` (`"True"`, a
    // jinja-repr map), which would be asymmetric with argv coercion
    // rejecting the same shapes outright.
    let stdin = match kwargs.get::<Option<MjValue>>("stdin")? {
        None => None,
        Some(v) => match v.as_str() {
            Some(s) => Some(s.to_string()),
            None => {
                return Err(Error::new(
                    ErrorKind::InvalidOperation,
                    "exec(): stdin= must be a string; use | tojson to pass structured data",
                ));
            }
        },
    };
    kwargs.assert_all_used()?;

    let req = crate::exec::ExecRequest {
        cmd,
        args: &argv,
        stdin: stdin.as_deref(),
        timeout_ms,
    };
    crate::exec::run(&req)
        .map(MjValue::from)
        .map_err(|e| Error::new(ErrorKind::InvalidOperation, e))
}

/// Coerce one `exec()` positional argument (beyond `cmd`) into an argv
/// string. `position` is the argument's 1-based position in the call, used
/// to name it in an error.
///
/// A string (or valid-UTF8 bytes) passes through as-is; a bool renders as
/// the literal `"true"`/`"false"` — not minijinja's `Display`, which prints
/// `True`/`False`; an integer or float renders in minijinja's canonical
/// string form; an undefined or none value is an error (most commonly a
/// missed daemon ref reaching `exec()` un-guarded); a sequence or map is an
/// error naming the argument and directing the caller to `stdin=` instead —
/// argv entries are scalars, not structured data.
fn coerce_exec_argv(v: &MjValue, position: usize) -> Result<String, Error> {
    if let Some(s) = v.as_str() {
        return Ok(s.to_string());
    }
    match v.kind() {
        ValueKind::Bool => Ok(if v.is_true() { "true" } else { "false" }.to_string()),
        ValueKind::Number => Ok(v.to_string()),
        ValueKind::Undefined | ValueKind::None => Err(Error::new(
            ErrorKind::InvalidOperation,
            format!("exec(): argument {position} is undefined or none"),
        )),
        _ => Err(Error::new(
            ErrorKind::InvalidOperation,
            format!(
                "exec(): argument {position} is not a scalar; pass structured data via stdin= instead"
            ),
        )),
    }
}

// ── Value conversion ──────────────────────────────────────────────────────────

/// Convert a minijinja `Value` to a `serde_json::Value`.
///
/// Preserves types: bool → bool, integer → number, string → string, map →
/// object, sequence → array. UNDEFINED / None → empty string (all-falsy
/// result, per canon `field_resolution.md` invariant 9: an all-empty cascade
/// resolves to `""`).
pub(crate) fn mj_to_json(v: MjValue) -> JsonValue {
    if v.is_undefined() || v.is_none() {
        return JsonValue::String(String::new());
    }
    // Try bool first (minijinja booleans are distinct from strings).
    if v.kind() == minijinja::value::ValueKind::Bool
        && let Ok(b) = v.clone().try_into() as Result<bool, _>
    {
        return JsonValue::Bool(b);
    }
    // Try integer.
    if v.kind() == minijinja::value::ValueKind::Number {
        if let Ok(n) = v.clone().try_into() as Result<i64, _> {
            return JsonValue::Number(serde_json::Number::from(n));
        }
        if let Ok(n) = v.clone().try_into() as Result<u64, _> {
            return JsonValue::Number(serde_json::Number::from(n));
        }
        if let Ok(f) = v.clone().try_into() as Result<f64, _>
            && let Some(n) = serde_json::Number::from_f64(f)
        {
            return JsonValue::Number(n);
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
        return json;
    }
    // String fallback — also handles UNDEFINED that slipped through.
    let s = v.to_string();
    JsonValue::String(s)
}

// ── exec() tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile and evaluate one expression against an empty context, the way
    /// [`crate::resolve`] evaluates the single-tag form. A missed ref is
    /// chainable undefined.
    fn eval_expr(expr: &str, allow_exec: bool) -> Result<JsonValue, String> {
        let env = build_expression_env(allow_exec);
        let compiled = env.compile_expression(expr).map_err(|e| e.to_string())?;
        let value = compiled
            .eval(MjValue::from_serialize(serde_json::json!({})))
            .map_err(|e| e.to_string())?;
        Ok(mj_to_json(value))
    }

    // Both forms of canon `field_resolution.md` invariant 14 share this one
    // environment, so pinning `exec()` here pins it for the template form
    // too; `libbeachcomber/tests/suite/resolve.rs` §9 runs both forms through the resolver.
    #[test]
    fn exec_enabled_env_runs_command_with_scalar_args() {
        let result = eval_expr(r#"exec("/bin/echo", "hi", 42)"#, true).unwrap();
        assert_eq!(result, JsonValue::String("hi 42".to_string()));
    }

    #[test]
    fn exec_argv_separation_is_pinned() {
        // `sh -c script arg0 arg1 arg2` sets $0 = arg0, so "_" fills $0 and
        // $1/$2 are "x"/"y" — $# must be 2, not 3, pinning that `cmd` +
        // remaining positionals map onto argv exactly (no accidental $0
        // duplication or off-by-one).
        let result = eval_expr(r#"exec("sh", "-c", "echo $#", "_", "x", "y")"#, true).unwrap();
        assert_eq!(result, JsonValue::String("2".to_string()));
    }

    #[test]
    fn exec_disabled_fn_names_the_context() {
        let err = eval_expr(r#"exec("/bin/echo", "hi")"#, false).unwrap_err();
        // minijinja's Display prefixes the error kind — assert the
        // substring, not the whole message.
        assert!(
            err.contains("exec() is disabled in this context"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn exec_undefined_arg_names_its_1_based_position() {
        // `cache.missing.value` is bound to nothing, so it evaluates as
        // chainable undefined — landing as exec()'s 2nd positional (1-based).
        let err = eval_expr(r#"exec("/bin/echo", cache.missing.value)"#, true).unwrap_err();
        assert!(err.contains("argument 2"), "unexpected error: {err}");
    }

    #[test]
    fn exec_stdin_kwarg_reaches_the_child() {
        let result = eval_expr(r#"exec("cat", stdin="round trip")"#, true).unwrap();
        assert_eq!(result, JsonValue::String("round trip".to_string()));
    }

    #[test]
    fn exec_unknown_kwarg_errors() {
        let err = eval_expr(r#"exec("/bin/echo", "hi", bogus=1)"#, true).unwrap_err();
        assert!(err.contains("bogus"), "unexpected error: {err}");
    }

    #[test]
    fn exec_bool_arg_coerces_to_lowercase_literal() {
        // minijinja's own `Display` for a bool prints "True"/"False"
        // (Python-style); exec() must emit the canonical "true"/"false".
        let result = eval_expr(r#"exec("/bin/echo", true, false)"#, true).unwrap();
        assert_eq!(result, JsonValue::String("true false".to_string()));
    }

    #[test]
    fn exec_stdin_rejects_non_string_value() {
        // A bool (or a map/seq) must not silently stringify through
        // minijinja's `Display` — symmetric with argv coercion, which
        // rejects the same shapes outright.
        let err = eval_expr(r#"exec("cat", stdin=true)"#, true).unwrap_err();
        assert!(err.contains("stdin"), "unexpected error: {err}");
        assert!(err.contains("tojson"), "unexpected error: {err}");
    }

    #[test]
    fn exec_timeout_ms_conversion_error_names_the_kwarg() {
        let err = eval_expr(r#"exec("/bin/echo", "hi", timeout_ms="x")"#, true).unwrap_err();
        assert!(err.contains("timeout_ms"), "unexpected error: {err}");
    }

    #[test]
    fn exec_seq_arg_is_rejected_with_its_position() {
        let err = eval_expr(r#"exec("/bin/echo", "hi", [1, 2])"#, true).unwrap_err();
        assert!(err.contains("argument 3"), "unexpected error: {err}");
        assert!(err.contains("stdin="), "unexpected error: {err}");
    }

    #[test]
    fn exec_non_string_cmd_errors() {
        let err = eval_expr(r#"exec(42)"#, true).unwrap_err();
        assert!(err.contains("argument 1"), "unexpected error: {err}");
    }
}
