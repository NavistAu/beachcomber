//! The minijinja environment every template and expression renders in.
//!
//! Two environments, each built once per process from the library table
//! ([`crate::library::ITEMS`]) by tier and handed out as a copy (a copy shares
//! what it holds, so it costs a few reference counts):
//!
//! - [`pure_engine_env`]: the pure items, with undefined values chainable. A
//!   surface that must do no IO (a provider's lines, a decorator render, run
//!   on every read) stops here.
//! - [`engine_env`]: the pure environment plus the items that read files and
//!   run processes (`read`, `lup`, `exec`) and memoise a leaf (`cache`,
//!   `watch`, `ttl`), and a clear refusal for the daemon's `url` and `plugin`.
//!   The daemon extends a copy with those two, so a Source's lines and a
//!   client template share one engine.
//!
//! Neither is bound to a render: an item reads the [`EngineCtx`] of the render
//! in progress on its thread ([`ctx`]), which the surface installs around its
//! render ([`with_ctx`]).

use crate::library::{self, Tier};
use minijinja::value::{Kwargs, Rest, Value as MjValue, ValueKind};
use minijinja::{AutoEscape, Environment, Error, ErrorKind, UndefinedBehavior};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

/// What a render knows about where it runs.
pub struct EngineCtx {
    /// The render's `env` map. Also the whole environment of an `exec` child.
    pub env: HashMap<String, String>,
    /// The render's directory: an `exec` child's working directory.
    pub cwd: Option<PathBuf>,
    /// Whether `exec` may run. Off, a call fails with a message saying so.
    pub allow_exec: bool,
    /// The clock `_.now()`, `ago` and `spinner` read: one value for the render.
    pub now: std::time::SystemTime,
    /// What the render's file items read (`read`, `stat`, `realpath`, `lup`,
    /// `glob`), each path stamped before it is read, where a daemon run keeps
    /// account: what a later read checks of the record it makes. `None`, a
    /// client's render, records nothing.
    pub touched: Option<Arc<crate::fs::Touched>>,
    /// The frames of the leaves a client's resolution is rendering, which
    /// `cache()`, `watch()` and `ttl()` read and record into
    /// ([`crate::library::memo`]). `None`, a daemon's render, refuses them.
    pub memo: Option<Arc<crate::library::memo::MemoFrames>>,
    /// Where the render's `log.*` lines are said to come from: a Source's
    /// `provider.source` in a daemon run, the leaf's address on a client.
    /// `None` is not deduplicated. Behind a lock because a client's resolution
    /// shares one context across its leaves ([`Self::set_origin`]).
    pub origin: std::sync::Mutex<Option<String>>,
}

/// A render with nothing: no environment, no directory, no `exec`, the clock
/// as it is now, no account of what it reads, and no leaf to memoise.
impl Default for EngineCtx {
    fn default() -> Self {
        Self {
            env: HashMap::new(),
            cwd: None,
            allow_exec: false,
            now: std::time::SystemTime::now(),
            touched: None,
            memo: None,
            origin: Default::default(),
        }
    }
}

impl EngineCtx {
    /// The render's origin label, if it has one.
    pub fn origin(&self) -> Option<String> {
        self.origin
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Set the origin label; the previous one comes back, to restore.
    pub fn set_origin(&self, origin: Option<String>) -> Option<String> {
        std::mem::replace(
            &mut *self.origin.lock().unwrap_or_else(|p| p.into_inner()),
            origin,
        )
    }

    /// `path` is about to be read: noted, where the render keeps account.
    pub fn touch(&self, path: &std::path::Path) {
        if let Some(touched) = &self.touched {
            touched.touch(path);
        }
    }

    /// Whether the colour filters draw escapes: unless the render's `env`
    /// sets `NO_COLOR` to something.
    pub fn colour(&self) -> bool {
        crate::term::colour_on(&self.env)
    }

    /// The render's clock as milliseconds since the Unix epoch.
    pub fn now_millis(&self) -> u128 {
        self.now
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis())
    }

    /// How many colours the render's terminal shows, from its `env`.
    pub fn depth(&self) -> crate::term::Depth {
        crate::term::Depth::of(&self.env)
    }
}

thread_local! {
    /// The contexts of the renders in progress on this thread, innermost last:
    /// a Source's line can call a Source, whose run renders its own.
    static CURRENT: RefCell<Vec<Arc<EngineCtx>>> = const { RefCell::new(Vec::new()) };
}

/// Run `f` with `ctx` as the context every engine item called on this thread
/// reads, until it returns (or unwinds).
pub fn with_ctx<R>(ctx: Arc<EngineCtx>, f: impl FnOnce() -> R) -> R {
    CURRENT.with(|c| c.borrow_mut().push(ctx));
    struct Pop;
    impl Drop for Pop {
        fn drop(&mut self) {
            CURRENT.with(|c| c.borrow_mut().pop());
        }
    }
    let _pop = Pop;
    f()
}

/// The context of the render in progress on this thread ([`with_ctx`]), or a
/// render with nothing ([`EngineCtx::default`]) where none is.
pub fn ctx() -> Arc<EngineCtx> {
    CURRENT
        .with(|c| c.borrow().last().cloned())
        .unwrap_or_default()
}

static PURE: LazyLock<Environment<'static>> = LazyLock::new(|| {
    let mut jinja = Environment::new();
    jinja.set_undefined_behavior(UndefinedBehavior::Chainable);
    jinja.set_auto_escape_callback(|_| AutoEscape::None);
    jinja.set_formatter(|out, state, value| {
        if value.is_none() {
            Ok(())
        } else {
            minijinja::escape_formatter(out, state, value)
        }
    });
    library::absent::guard_builtins(&mut jinja);
    library::install(&mut jinja, Tier::Pure);
    library::gather(&mut jinja);
    jinja
});

static FULL: LazyLock<Environment<'static>> = LazyLock::new(|| {
    let mut jinja = PURE.clone();
    for tier in [Tier::Clock, Tier::Io, Tier::Exec, Tier::Memo, Tier::Daemon] {
        library::install(&mut jinja, tier);
    }
    library::gather(&mut jinja);
    jinja
});

/// The pure library, with undefined values chainable.
///
/// `Chainable` because a missing ref is falsy at any depth: `Lenient` renders
/// an undefined value as empty but *errors* on attribute access into one, so
/// `{{ p.f.sub }}` would blow up whenever `p.f` missed while `{{ p.f }}`
/// quietly rendered nothing.
///
/// No template's name turns on escaping: the engine would escape a template
/// named like a file (`*.html`, `*.json`), and a macro library's name is an
/// address (`p:macro.json`), not a file. A `none` printed inside text is
/// nothing, never the word `none`: every environment comes from here, so
/// this is the one place that rule is installed.
pub fn pure_engine_env() -> Environment<'static> {
    PURE.clone()
}

/// The pure environment plus every item that reads files, runs processes or
/// memoises a leaf, and the client's refusal of `url` and `plugin`. A
/// daemon's render has no memo frames, so its `cache`, `watch` and `ttl`
/// refuse with their reason.
///
/// `exec` is registered whether or not it is allowed, so a disabled call fails
/// with the on-topic "exec() is disabled in this context" rather than a
/// generic "unknown function".
pub fn engine_env() -> Environment<'static> {
    FULL.clone()
}

/// Install `exec`, as a function and a filter.
pub(crate) fn add_exec(jinja: &mut Environment<'_>) {
    add_command(jinja, "exec");
}

/// Install `execa`, as a function and a filter.
pub(crate) fn add_execa(jinja: &mut Environment<'_>) {
    add_command(jinja, "execa");
}

fn add_command(jinja: &mut Environment<'_>, name: &'static str) {
    jinja.add_function(
        name,
        move |args: Rest<MjValue>, kwargs: Kwargs| match args.as_slice() {
            [command] => exec(&ctx(), name, command, kwargs),
            _ => Err(op_error(format!(
                "{name}(): one command — a string for sh -c, or a list for argv"
            ))),
        },
    );
    jinja.add_filter(
        name,
        move |piped: MjValue, args: Rest<MjValue>, kwargs: Kwargs| {
            if !args.is_empty() {
                return Err(op_error(format!(
                    "{name}(): a command from both the pipe and an argument"
                )));
            }
            exec(&ctx(), name, &piped, kwargs)
        },
    );
}

/// An `InvalidOperation` error, the one every engine function fails with.
pub fn op_error(message: impl Into<std::borrow::Cow<'static, str>>) -> Error {
    Error::new(ErrorKind::InvalidOperation, message)
}

/// How long `_.exec()` and `_.url()` wait when their `timeout=` does not say.
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
            "{function}(): timeout: {}",
            crate::duration::refuse_below("1ms")
        ))),
        Err(e) => Err(op_error(format!("{function}(): timeout: {e}"))),
    }
}

/// `_.exec(cmd, timeout='10s', stdin=none, sh='sh')` and
/// `_.execa(cmd, ok=none, timeout=, stdin=, sh=)` (`name` says which), the one
/// entry behind both spellings of each. A string runs `sh -c`; a list is
/// argv, no shell. The child's environment is `ctx.env` and its directory
/// `ctx.cwd`. `exec` fails on a non-zero exit and returns stdout; `execa`
/// returns `{stdout, stderr, exit_code, signal}` for any exit `ok=` (a list of
/// exit codes, any when absent) accepts.
fn exec(ctx: &EngineCtx, name: &str, command: &MjValue, kwargs: Kwargs) -> Result<MjValue, Error> {
    if !ctx.allow_exec {
        return Err(op_error("exec() is disabled in this context"));
    }
    let timeout = timeout_kwarg(&kwargs, name)?;
    let ok = if name == "execa" {
        ok_kwarg(&kwargs)?
    } else {
        None
    };
    // A raw `Value`, so a non-string (`stdin=true`, a map) is rejected rather
    // than stringified through minijinja's `Display`.
    let stdin = match kwargs.get::<Option<MjValue>>("stdin")? {
        None => None,
        Some(v) => match v.as_str() {
            Some(s) => Some(s.to_string()),
            None => {
                return Err(op_error(format!(
                    "{name}(): stdin= must be a string; use | tojson to pass structured data"
                )));
            }
        },
    };
    let shell = kwargs.get::<Option<String>>("sh")?;
    kwargs.assert_all_used()?;

    let (program, args) = if let Some(script) = command.as_str() {
        let shell = shell.unwrap_or_else(|| "sh".to_string());
        (shell, vec!["-c".to_string(), script.to_string()])
    } else if command.kind() == ValueKind::Seq {
        if shell.is_some() {
            return Err(op_error(format!(
                "{name}(): sh= applies to a string command only"
            )));
        }
        let mut argv = Vec::new();
        for (i, v) in command.try_iter()?.enumerate() {
            argv.push(coerce_argv(name, &v, i + 1)?);
        }
        if argv.is_empty() {
            return Err(op_error(format!("{name}(): the command list is empty")));
        }
        (argv.remove(0), argv)
    } else {
        return Err(op_error(format!(
            "{name}(): the command must be a string for sh -c, or a list for argv"
        )));
    };

    let req = crate::exec::ExecRequest {
        cmd: &program,
        args: &args,
        stdin: stdin.as_deref(),
        timeout,
        env: &ctx.env,
        cwd: ctx.cwd.as_deref(),
    };
    if name != "execa" {
        return crate::exec::run(&req).map(MjValue::from).map_err(op_error);
    }
    // A signal has no exit code, so it is outside any `ok=` list.
    let accept = |status: &std::process::ExitStatus| {
        ok.as_ref()
            .is_none_or(|codes| status.code().is_some_and(|c| codes.contains(&c)))
    };
    let ran = crate::exec::run_with(&req, accept).map_err(op_error)?;
    Ok(minijinja::context! {
        stdout => ran.stdout,
        stderr => ran.stderr,
        exit_code => ran.code,
        signal => ran.signal,
    })
}

/// `execa`'s `ok=`: the exit codes that are not a failure, none when absent.
fn ok_kwarg(kwargs: &Kwargs) -> Result<Option<Vec<i32>>, Error> {
    let Some(value) = kwargs.get::<Option<MjValue>>("ok")? else {
        return Ok(None);
    };
    let bad = || op_error("execa(): ok= must be a list of integers, such as ok=[0, 1]");
    if value.kind() != ValueKind::Seq {
        return Err(bad());
    }
    value
        .try_iter()?
        .map(|v| {
            i32::try_from(v.clone())
                .ok()
                .filter(|_| v.kind() == ValueKind::Number)
                .ok_or_else(bad)
        })
        .collect::<Result<_, _>>()
        .map(Some)
}

/// Coerce one list element into an argv string; `position` is its 1-based
/// place in the list.
///
/// A string passes through; a bool renders `true`/`false` (not minijinja's
/// `True`/`False`); a number renders canonically; undefined or none is an
/// error (most often a missed ref reaching `_.exec()` unguarded); a sequence
/// or map is an error pointing at `stdin=`.
fn coerce_argv(name: &str, v: &MjValue, position: usize) -> Result<String, Error> {
    if let Some(s) = v.as_str() {
        return Ok(s.to_string());
    }
    match v.kind() {
        ValueKind::Bool => Ok(if v.is_true() { "true" } else { "false" }.to_string()),
        ValueKind::Number => Ok(v.to_string()),
        ValueKind::Undefined | ValueKind::None => Err(op_error(format!(
            "{name}(): argument {position} is undefined or none"
        ))),
        _ => Err(op_error(format!(
            "{name}(): argument {position} is not a scalar; pass structured data via stdin= instead"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::testing::{ctx_with, eval, eval_in};

    /// A template's name never turns on escaping: a macro library is named by
    /// its address, and `p:macro.html` or `p:macro.json` is no file.
    #[test]
    fn a_template_name_never_turns_on_escaping() {
        let builders: [fn() -> Environment<'static>; 2] = [pure_engine_env, engine_env];
        let text = r#"<a href="x">&'</a>"#;
        for build in builders {
            let mut env = build();
            for name in [
                "p:macro.html",
                "p:macro.json",
                "x.htm",
                "x.xml",
                "x.yaml",
                "x.js",
            ] {
                env.add_template_owned(name.to_string(), "{{ v }}").unwrap();
                let out = env
                    .get_template(name)
                    .unwrap()
                    .render(minijinja::context!(v => text))
                    .unwrap();
                assert_eq!(out, text, "{name}");
            }
        }
    }

    #[test]
    fn exec_string_runs_under_sh() {
        let ctx = ctx_with(&[("FOO", "bar")], None);
        assert_eq!(eval_in(ctx, "_.exec('echo $FOO')").unwrap(), "bar");
        assert_eq!(eval("_.exec('echo a | tr a b')").unwrap(), "b");
    }

    #[test]
    fn exec_list_does_not_expand() {
        let ctx = ctx_with(&[("FOO", "bar")], None);
        assert_eq!(eval_in(ctx, "_.exec(['echo', '$FOO'])").unwrap(), "$FOO");
    }

    #[test]
    fn exec_list_coerces_scalars_and_names_the_bad_position() {
        assert_eq!(eval("_.exec(['echo', 1, true])").unwrap(), "1 true");
        let err = eval("_.exec(['echo', none])").unwrap_err();
        assert!(err.contains("argument 2"), "unexpected error: {err}");
    }

    #[test]
    fn exec_child_env_is_the_render_env_not_the_process_env() {
        // The test process has HOME; the render's env does not.
        assert!(std::env::var_os("HOME").is_some());
        assert_eq!(eval("_.exec('echo \"[$HOME]\"')").unwrap(), "[]");
    }

    #[test]
    fn exec_runs_in_the_ctx_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let want = dir.path().canonicalize().unwrap();
        let ctx = ctx_with(&[], Some(&want));
        assert_eq!(
            eval_in(ctx, "_.exec('pwd')").unwrap(),
            want.to_str().unwrap()
        );
    }

    #[test]
    fn exec_filter_form_matches_the_function() {
        assert_eq!(eval("'echo hi' | exec()").unwrap(), "hi");
        assert_eq!(eval("['echo', 'a b'] | exec()").unwrap(), "a b");
        assert_eq!(
            eval("'cat' | exec(stdin='piped')").unwrap(),
            eval("_.exec('cat', stdin='piped')").unwrap()
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
        let err = eval("_.exec('echo', 'hi')").unwrap_err();
        assert!(
            err.contains("one command — a string for sh -c, or a list for argv"),
            "unexpected error: {err}"
        );
        assert!(eval("_.exec()").is_err());
    }

    #[test]
    fn exec_sh_kwarg_picks_the_shell() {
        // `$0` is the shell as invoked, so it names the one that ran.
        assert_eq!(eval("_.exec('echo $0', sh='bash')").unwrap(), "bash");
    }

    #[test]
    fn exec_non_string_non_list_command_errors() {
        let err = eval("_.exec(42)").unwrap_err();
        assert!(err.contains("string"), "unexpected error: {err}");
    }

    #[test]
    fn exec_stdin_kwarg_reaches_the_child() {
        assert_eq!(
            eval("_.exec('cat', stdin='round trip')").unwrap(),
            "round trip"
        );
    }

    #[test]
    fn exec_stdin_takes_structured_data_through_tojson() {
        assert_eq!(
            eval(r#"_.exec('cat', stdin=({"a": [1, "x"]} | tojson))"#).unwrap(),
            r#"{"a": [1, "x"]}"#
        );
    }

    #[test]
    fn exec_unknown_kwarg_errors() {
        let err = eval("_.exec('echo hi', bogus=1)").unwrap_err();
        assert!(err.contains("bogus"), "unexpected error: {err}");
    }

    #[test]
    fn exec_stdin_rejects_non_string_value() {
        let err = eval("_.exec('cat', stdin=true)").unwrap_err();
        assert!(err.contains("stdin") && err.contains("tojson"), "{err}");
    }

    /// `timeout=` is a duration in the one grammar: a string, or a bare
    /// number of seconds.
    #[test]
    fn exec_timeout_is_a_duration() {
        assert_eq!(eval("_.exec('echo hi', timeout='2s')").unwrap(), "hi");
        assert_eq!(eval("_.exec('echo hi', timeout=2)").unwrap(), "hi");
        let err = eval("_.exec('sleep 2', timeout='10ms')").unwrap_err();
        assert!(err.contains("timed out after 10ms"), "{err}");
        for bad in ["'x'", "'0s'", "true"] {
            let err = eval(&format!("_.exec('echo hi', timeout={bad})")).unwrap_err();
            assert!(err.contains("exec(): timeout: "), "{bad}: {err}");
        }
    }

    /// The millisecond keyword is gone, and says what took its place.
    #[test]
    fn exec_timeout_ms_names_its_replacement() {
        let err = eval("_.exec('echo hi', timeout_ms=5000)").unwrap_err();
        assert!(
            err.contains("exec(): timeout_ms= is removed: write timeout='10s'"),
            "{err}"
        );
    }

    #[test]
    fn exec_string_failure_names_the_script() {
        let err = eval("_.exec('kubectl-nope config current-context')").unwrap_err();
        assert!(
            err.contains("`sh -c 'kubectl-nope config current-context'`"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn exec_list_failure_names_the_program() {
        let err = eval("_.exec(['false'])").unwrap_err();
        assert!(err.contains("`false`"), "unexpected error: {err}");
    }

    #[test]
    fn exec_missing_cwd_says_the_directory_is_missing() {
        let missing = std::path::Path::new("/nonexistent-comb-cwd");
        let ctx = ctx_with(&[], Some(missing));
        let err = eval_in(ctx, "_.exec('echo hi')").unwrap_err();
        assert!(
            err.contains("failed to spawn in /nonexistent-comb-cwd"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn exec_disabled_names_the_context_in_both_forms() {
        let ctx = Arc::new(EngineCtx::default());
        for expr in ["_.exec('echo hi')", "'echo hi' | exec()"] {
            let err = eval_in(ctx.clone(), expr).unwrap_err();
            assert!(
                err.contains("exec() is disabled in this context"),
                "unexpected error: {err}"
            );
        }
    }

    #[test]
    fn execa_returns_a_map_for_any_exit() {
        use crate::library::testing::eval_json;
        assert_eq!(
            eval_json("'echo hi; echo oops >&2' | execa()"),
            r#"{"stdout":"hi","stderr":"oops","exit_code":0,"signal":null}"#
        );
        assert_eq!(
            eval_json("'echo o; echo e >&2; exit 1' | execa()"),
            r#"{"stdout":"o","stderr":"e","exit_code":1,"signal":null}"#
        );
        assert_eq!(
            eval_json("_.execa(['sh', '-c', 'echo a b'])"),
            r#"{"stdout":"a b","stderr":"","exit_code":0,"signal":null}"#
        );
    }

    #[test]
    fn execa_signal_has_no_exit_code() {
        use crate::library::testing::eval_json;
        assert_eq!(
            eval_json("'kill -9 $$' | execa()"),
            r#"{"stdout":"","stderr":"","exit_code":null,"signal":9}"#
        );
        assert_eq!(
            eval("('kill -9 $$' | execa()).exit_code is none").unwrap(),
            "True"
        );
        let err = eval("'kill -9 $$' | execa(ok=[0])").unwrap_err();
        assert!(err.contains("signal: 9"), "{err}");
    }

    #[test]
    fn execa_ok_list_fails_like_exec() {
        let err = eval("_.execa('exit 4', ok=[0, 1])").unwrap_err();
        assert!(err.contains("`sh -c 'exit 4'`: exit status: 4"), "{err}");
        assert_eq!(eval("_.execa('exit 1', ok=[0, 1]).exit_code").unwrap(), "1");
        let err = eval("_.execa('echo x >&2; exit 3', ok=[0])").unwrap_err();
        assert!(err.contains("exit status: 3: x"), "{err}");
    }

    #[test]
    fn execa_bad_ok_is_an_error() {
        for bad in ["0", "'0'", "['a']", "[0.5]", "[true]"] {
            let err = eval(&format!("_.execa('true', ok={bad})")).unwrap_err();
            assert!(err.contains("execa(): ok="), "{bad}: {err}");
        }
        assert!(eval("_.exec('true', ok=[0])").is_err());
    }

    #[test]
    fn execa_timeout_fails() {
        let err = eval("_.execa('sleep 5', timeout='50ms')").unwrap_err();
        assert!(err.contains("timed out after 50ms"), "{err}");
    }

    #[test]
    fn execa_disabled_names_the_context_in_both_forms() {
        let ctx = Arc::new(EngineCtx::default());
        for expr in ["_.execa('echo hi')", "'echo hi' | execa()"] {
            let err = eval_in(ctx.clone(), expr).unwrap_err();
            assert!(
                err.contains("exec() is disabled in this context"),
                "unexpected error: {err}"
            );
        }
    }
}
