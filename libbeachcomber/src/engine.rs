//! The minijinja environment every template and expression renders in.
//!
//! Two builders, both installing from the library table
//! ([`crate::library::ITEMS`]) by tier:
//!
//! - [`build_pure_engine_env`]: the pure items, with undefined values
//!   chainable. A surface that must do no IO (a provider's lines, a decorator
//!   render, run on every read) stops here.
//! - [`build_engine_env`]: the pure environment plus the items that read files
//!   and run processes (`read`, `lup`, `exec`), bound to the render's
//!   [`EngineCtx`], and a clear refusal for the daemon's `url` and `plugin`.
//!   The daemon extends the returned environment with those two, so a Source's
//!   lines and a client template share one engine.

use crate::library::{self, Tier};
use minijinja::value::{Kwargs, Rest, Value as MjValue, ValueKind};
use minijinja::{Environment, Error, ErrorKind, UndefinedBehavior};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// What a render knows about where it runs.
#[derive(Default)]
pub struct EngineCtx {
    /// The render's `env` map. Also the whole environment of an `exec` child.
    pub env: HashMap<String, String>,
    /// The render's directory: an `exec` child's working directory.
    pub cwd: Option<PathBuf>,
    /// Whether `exec` may run. Off, a call fails with a message saying so.
    pub allow_exec: bool,
}

impl EngineCtx {
    /// Whether the colour filters draw escapes: unless the render's `env`
    /// sets `NO_COLOR` to something.
    pub fn colour(&self) -> bool {
        self.env.get("NO_COLOR").is_none_or(|v| v.is_empty())
    }
}

/// The pure library, with undefined values chainable, bound to `ctx`.
///
/// `Chainable` because a missing ref is falsy at any depth: `Lenient` renders
/// an undefined value as empty but *errors* on attribute access into one, so
/// `{{ p.f.sub }}` would blow up whenever `p.f` missed while `{{ p.f }}`
/// quietly rendered nothing.
pub fn build_pure_engine_env<'a>(ctx: Arc<EngineCtx>) -> Environment<'a> {
    let mut jinja = Environment::new();
    jinja.set_undefined_behavior(UndefinedBehavior::Chainable);
    library::install(&mut jinja, &ctx, Tier::Pure);
    jinja
}

/// The pure environment plus every item that reads files or runs processes,
/// bound to `ctx`, and the client's refusal of `url` and `plugin`.
///
/// `exec` is registered whether or not it is allowed, so a disabled call fails
/// with the on-topic "exec() is disabled in this context" rather than a
/// generic "unknown function".
pub fn build_engine_env<'a>(ctx: Arc<EngineCtx>) -> Environment<'a> {
    let mut jinja = build_pure_engine_env(ctx.clone());
    for tier in [Tier::Io, Tier::Exec, Tier::Daemon] {
        library::install(&mut jinja, &ctx, tier);
    }
    jinja
}

/// Install `exec`, as a function and a filter, bound to `ctx`.
pub(crate) fn add_exec(jinja: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    let for_fn = ctx.clone();
    let for_filter = ctx.clone();
    jinja.add_function(
        "exec",
        move |args: Rest<MjValue>, kwargs: Kwargs| match args.as_slice() {
            [command] => exec(&for_fn, command, kwargs),
            _ => Err(op_error(
                "exec(): one command — a string for sh -c, or a list for argv",
            )),
        },
    );
    jinja.add_filter(
        "exec",
        move |piped: MjValue, args: Rest<MjValue>, kwargs: Kwargs| {
            if !args.is_empty() {
                return Err(op_error(
                    "exec(): a command from both the pipe and an argument",
                ));
            }
            exec(&for_filter, &piped, kwargs)
        },
    );
}

/// An `InvalidOperation` error, the one every engine function fails with.
pub fn op_error(message: impl Into<std::borrow::Cow<'static, str>>) -> Error {
    Error::new(ErrorKind::InvalidOperation, message)
}

/// How long `exec()` and `url()` wait when their `timeout=` does not say.
pub const DEFAULT_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The `timeout=` keyword of `function` (`exec`, `url`): a duration in the one
/// grammar ([`crate::duration`]) of at least 1ms, [`DEFAULT_CALL_TIMEOUT`]
/// when absent. The millisecond keyword it replaced says what took its place.
pub fn timeout_kwarg(kwargs: &Kwargs, function: &str) -> Result<std::time::Duration, Error> {
    if kwargs.has("timeout_ms") {
        return Err(op_error(format!(
            "{function}(): timeout_ms= is removed: write timeout='10s'"
        )));
    }
    let Some(value) = kwargs.get::<Option<MjValue>>("timeout")? else {
        return Ok(DEFAULT_CALL_TIMEOUT);
    };
    match crate::duration::from_value(&value) {
        Ok(d) if !d.is_zero() => Ok(d),
        Ok(_) => Err(op_error(format!(
            "{function}(): timeout= must be at least 1ms"
        ))),
        Err(e) => Err(op_error(format!("{function}(): timeout= {e}"))),
    }
}

/// `exec(cmd, timeout='10s', stdin=none, sh='sh')`, the one entry behind
/// both spellings. A string runs `sh -c`; a list is argv, no shell. The
/// child's environment is `ctx.env` and its directory `ctx.cwd`.
fn exec(ctx: &EngineCtx, command: &MjValue, kwargs: Kwargs) -> Result<MjValue, Error> {
    if !ctx.allow_exec {
        return Err(op_error("exec() is disabled in this context"));
    }
    let timeout = timeout_kwarg(&kwargs, "exec")?;
    // A raw `Value`, so a non-string (`stdin=true`, a map) is rejected rather
    // than stringified through minijinja's `Display`.
    let stdin = match kwargs.get::<Option<MjValue>>("stdin")? {
        None => None,
        Some(v) => match v.as_str() {
            Some(s) => Some(s.to_string()),
            None => {
                return Err(op_error(
                    "exec(): stdin= must be a string; use | tojson to pass structured data",
                ));
            }
        },
    };
    let shell = kwargs.get::<Option<String>>("sh")?;
    kwargs.assert_all_used()?;

    let mut label = None;
    let (program, args) = if let Some(script) = command.as_str() {
        let shell = shell.unwrap_or_else(|| "sh".to_string());
        label = Some(format!("{shell} -c '{script}'"));
        (shell, vec!["-c".to_string(), script.to_string()])
    } else if command.kind() == ValueKind::Seq {
        if shell.is_some() {
            return Err(op_error("exec(): sh= applies to a string command only"));
        }
        let mut argv = Vec::new();
        for (i, v) in command.try_iter()?.enumerate() {
            argv.push(coerce_argv(&v, i + 1)?);
        }
        if argv.is_empty() {
            return Err(op_error("exec(): the command list is empty"));
        }
        (argv.remove(0), argv)
    } else {
        return Err(op_error(
            "exec(): the command must be a string for sh -c, or a list for argv",
        ));
    };

    let req = crate::exec::ExecRequest {
        cmd: &program,
        label: label.as_deref(),
        args: &args,
        stdin: stdin.as_deref(),
        timeout,
        env: &ctx.env,
        cwd: ctx.cwd.as_deref(),
    };
    crate::exec::run(&req).map(MjValue::from).map_err(op_error)
}

/// Coerce one list element into an argv string; `position` is its 1-based
/// place in the list.
///
/// A string passes through; a bool renders `true`/`false` (not minijinja's
/// `True`/`False`); a number renders canonically; undefined or none is an
/// error (most often a missed ref reaching `exec()` unguarded); a sequence
/// or map is an error pointing at `stdin=`.
fn coerce_argv(v: &MjValue, position: usize) -> Result<String, Error> {
    if let Some(s) = v.as_str() {
        return Ok(s.to_string());
    }
    match v.kind() {
        ValueKind::Bool => Ok(if v.is_true() { "true" } else { "false" }.to_string()),
        ValueKind::Number => Ok(v.to_string()),
        ValueKind::Undefined | ValueKind::None => Err(op_error(format!(
            "exec(): argument {position} is undefined or none"
        ))),
        _ => Err(op_error(format!(
            "exec(): argument {position} is not a scalar; pass structured data via stdin= instead"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::testing::{ctx_with, eval, eval_in};

    #[test]
    fn exec_string_runs_under_sh() {
        let ctx = ctx_with(&[("FOO", "bar")], None);
        assert_eq!(eval_in(ctx, "exec('echo $FOO')").unwrap(), "bar");
        assert_eq!(eval("exec('echo a | tr a b')").unwrap(), "b");
    }

    #[test]
    fn exec_list_does_not_expand() {
        let ctx = ctx_with(&[("FOO", "bar")], None);
        assert_eq!(eval_in(ctx, "exec(['echo', '$FOO'])").unwrap(), "$FOO");
    }

    #[test]
    fn exec_list_coerces_scalars_and_names_the_bad_position() {
        assert_eq!(eval("exec(['echo', 1, true])").unwrap(), "1 true");
        let err = eval("exec(['echo', none])").unwrap_err();
        assert!(err.contains("argument 2"), "unexpected error: {err}");
    }

    #[test]
    fn exec_child_env_is_the_render_env_not_the_process_env() {
        // The test process has HOME; the render's env does not.
        assert!(std::env::var_os("HOME").is_some());
        assert_eq!(eval("exec('echo \"[$HOME]\"')").unwrap(), "[]");
    }

    #[test]
    fn exec_runs_in_the_ctx_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let want = dir.path().canonicalize().unwrap();
        let ctx = ctx_with(&[], Some(&want));
        assert_eq!(eval_in(ctx, "exec('pwd')").unwrap(), want.to_str().unwrap());
    }

    #[test]
    fn exec_filter_form_matches_the_function() {
        assert_eq!(eval("'echo hi' | exec()").unwrap(), "hi");
        assert_eq!(eval("['echo', 'a b'] | exec()").unwrap(), "a b");
        assert_eq!(
            eval("'cat' | exec(stdin='piped')").unwrap(),
            eval("exec('cat', stdin='piped')").unwrap()
        );
    }

    #[test]
    fn exec_from_pipe_and_argument_is_an_error() {
        let err = eval("'echo a' | exec('echo b')").unwrap_err();
        assert!(
            err.contains("a command from both the pipe and an argument"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn exec_variadic_call_is_an_error() {
        let err = eval("exec('echo', 'hi')").unwrap_err();
        assert!(
            err.contains("one command — a string for sh -c, or a list for argv"),
            "unexpected error: {err}"
        );
        assert!(eval("exec()").is_err());
    }

    #[test]
    fn exec_sh_kwarg_picks_the_shell() {
        // `$0` is the shell as invoked, so it names the one that ran.
        assert_eq!(eval("exec('echo $0', sh='bash')").unwrap(), "bash");
    }

    #[test]
    fn exec_non_string_non_list_command_errors() {
        let err = eval("exec(42)").unwrap_err();
        assert!(err.contains("string"), "unexpected error: {err}");
    }

    #[test]
    fn exec_stdin_kwarg_reaches_the_child() {
        assert_eq!(
            eval("exec('cat', stdin='round trip')").unwrap(),
            "round trip"
        );
    }

    #[test]
    fn exec_stdin_takes_structured_data_through_tojson() {
        assert_eq!(
            eval(r#"exec('cat', stdin=({"a": [1, "x"]} | tojson))"#).unwrap(),
            r#"{"a":[1,"x"]}"#
        );
    }

    #[test]
    fn exec_unknown_kwarg_errors() {
        let err = eval("exec('echo hi', bogus=1)").unwrap_err();
        assert!(err.contains("bogus"), "unexpected error: {err}");
    }

    #[test]
    fn exec_stdin_rejects_non_string_value() {
        let err = eval("exec('cat', stdin=true)").unwrap_err();
        assert!(err.contains("stdin") && err.contains("tojson"), "{err}");
    }

    /// `timeout=` is a duration in the one grammar: a string, or a bare
    /// number of seconds.
    #[test]
    fn exec_timeout_is_a_duration() {
        assert_eq!(eval("exec('echo hi', timeout='2s')").unwrap(), "hi");
        assert_eq!(eval("exec('echo hi', timeout=2)").unwrap(), "hi");
        let err = eval("exec('sleep 2', timeout='100ms')").unwrap_err();
        assert!(err.contains("timed out after 100ms"), "{err}");
        for bad in ["'x'", "'0s'", "true"] {
            let err = eval(&format!("exec('echo hi', timeout={bad})")).unwrap_err();
            assert!(err.contains("exec(): timeout="), "{bad}: {err}");
        }
    }

    /// The millisecond keyword is gone, and says what took its place.
    #[test]
    fn exec_timeout_ms_names_its_replacement() {
        let err = eval("exec('echo hi', timeout_ms=5000)").unwrap_err();
        assert!(
            err.contains("exec(): timeout_ms= is removed: write timeout='10s'"),
            "{err}"
        );
    }

    #[test]
    fn exec_string_failure_names_the_script() {
        let err = eval("exec('kubectl-nope config current-context')").unwrap_err();
        assert!(
            err.contains("`sh -c 'kubectl-nope config current-context'`"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn exec_list_failure_names_the_program() {
        let err = eval("exec(['false'])").unwrap_err();
        assert!(err.contains("`false`"), "unexpected error: {err}");
    }

    #[test]
    fn exec_missing_cwd_says_the_directory_is_missing() {
        let missing = std::path::Path::new("/nonexistent-comb-cwd");
        let ctx = ctx_with(&[], Some(missing));
        let err = eval_in(ctx, "exec('echo hi')").unwrap_err();
        assert!(
            err.contains("failed to spawn in /nonexistent-comb-cwd"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn exec_disabled_names_the_context_in_both_forms() {
        let ctx = Arc::new(EngineCtx::default());
        for expr in ["exec('echo hi')", "'echo hi' | exec()"] {
            let err = eval_in(ctx.clone(), expr).unwrap_err();
            assert!(
                err.contains("exec() is disabled in this context"),
                "unexpected error: {err}"
            );
        }
    }
}
