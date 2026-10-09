//! The minijinja environment every template and expression renders in.
//!
//! [`build_engine_env`] is the one builder: the shared filters, chainable
//! undefined, and `exec` (a function and a filter) bound to the render's
//! [`EngineCtx`]. The daemon extends the returned environment with its own
//! functions, so a Source's lines and a client template share one engine.

use crate::filters::build_env;
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

/// The shared filters, chainable undefined, and `exec` as both a function
/// and a filter, bound to `ctx`.
///
/// `Chainable` because a missing ref is falsy at any depth: `Lenient` renders
/// an undefined value as empty but *errors* on attribute access into one, so
/// `{{ p.f.sub }}` would blow up whenever `p.f` missed while `{{ p.f }}`
/// quietly rendered nothing. Both modes register *some* `exec` so a disabled
/// call fails with the on-topic "exec() is disabled in this context" rather
/// than a generic "unknown function".
pub fn build_engine_env<'a>(ctx: Arc<EngineCtx>) -> Environment<'a> {
    let mut env = build_env();
    env.set_undefined_behavior(UndefinedBehavior::Chainable);
    let for_fn = ctx.clone();
    env.add_function(
        "exec",
        move |args: Rest<MjValue>, kwargs: Kwargs| match args.as_slice() {
            [command] => exec(&for_fn, command, kwargs),
            _ => Err(op_error(
                "exec(): one command — a string for sh -c, or a list for argv",
            )),
        },
    );
    env.add_filter(
        "exec",
        move |piped: MjValue, args: Rest<MjValue>, kwargs: Kwargs| {
            if !args.is_empty() {
                return Err(op_error(
                    "exec(): a command from both the pipe and an argument",
                ));
            }
            exec(&ctx, &piped, kwargs)
        },
    );
    env
}

fn op_error(message: impl Into<std::borrow::Cow<'static, str>>) -> Error {
    Error::new(ErrorKind::InvalidOperation, message)
}

/// `exec(cmd, timeout_ms=10000, stdin=none, sh='sh')`, the one entry behind
/// both spellings. A string runs `sh -c`; a list is argv, no shell. The
/// child's environment is `ctx.env` and its directory `ctx.cwd`.
fn exec(ctx: &EngineCtx, command: &MjValue, kwargs: Kwargs) -> Result<MjValue, Error> {
    if !ctx.allow_exec {
        return Err(op_error("exec() is disabled in this context"));
    }
    // A bare `?` on a conversion failure (e.g. `timeout_ms="x"`) would say
    // "cannot convert string to u64", naming neither `exec()` nor the keyword.
    let timeout_ms = kwargs
        .get::<Option<u64>>("timeout_ms")
        .map_err(|e| {
            op_error(format!(
                "exec(): timeout_ms= {}",
                e.detail().unwrap_or("invalid value")
            ))
        })?
        .unwrap_or(10_000);
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
        timeout_ms,
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

    fn ctx_with(env: &[(&str, &str)], cwd: Option<&std::path::Path>) -> Arc<EngineCtx> {
        let mut map = HashMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())]);
        map.extend(env.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        Arc::new(EngineCtx {
            env: map,
            cwd: cwd.map(Into::into),
            allow_exec: true,
        })
    }

    fn eval_in(ctx: Arc<EngineCtx>, expr: &str) -> Result<String, String> {
        let mut env = build_engine_env(ctx);
        env.set_undefined_behavior(UndefinedBehavior::Chainable);
        let compiled = env.compile_expression(expr).map_err(|e| e.to_string())?;
        let value = compiled
            .eval(MjValue::from_serialize(serde_json::json!({})))
            .map_err(|e| e.to_string())?;
        Ok(value.to_string())
    }

    fn eval(expr: &str) -> Result<String, String> {
        eval_in(ctx_with(&[], None), expr)
    }

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
    fn exec_unknown_kwarg_errors() {
        let err = eval("exec('echo hi', bogus=1)").unwrap_err();
        assert!(err.contains("bogus"), "unexpected error: {err}");
    }

    #[test]
    fn exec_stdin_rejects_non_string_value() {
        let err = eval("exec('cat', stdin=true)").unwrap_err();
        assert!(err.contains("stdin") && err.contains("tojson"), "{err}");
    }

    #[test]
    fn exec_timeout_ms_conversion_error_names_the_kwarg() {
        let err = eval("exec('echo hi', timeout_ms='x')").unwrap_err();
        assert!(err.contains("timeout_ms"), "unexpected error: {err}");
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
