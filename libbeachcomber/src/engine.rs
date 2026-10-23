//! The minijinja environment every template and expression renders in.
//!
//! [`build_engine_env`] is the one builder: the shared filters, chainable
//! undefined, and `exec` (a function and a filter) bound to the render's
//! [`EngineCtx`], plus the data functions a Source's lines are written in
//! (`read`, `lup`, `dirname`, `pluck`, `fromini`, `merge`, `map` over
//! objects). The daemon extends the returned environment with its own
//! functions, so a Source's lines and a client template share one engine.

use crate::filters::build_env;
use minijinja::value::{Kwargs, Rest, Value as MjValue, ValueKind};
use minijinja::{Environment, Error, ErrorKind, State, UndefinedBehavior};
use std::collections::HashMap;
use std::os::unix::ffi::OsStrExt;
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
    let for_exec_filter = ctx.clone();
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
            exec(&for_exec_filter, &piped, kwargs)
        },
    );
    let for_filter = ctx.clone();
    env.add_filter("read", move |path: MjValue| read(&for_filter, &path));
    env.add_function("lup", move |name: String, kwargs: Kwargs| {
        lup(&ctx, &name, kwargs)
    });
    env.add_filter("dirname", dirname);
    env.add_filter("pluck", pluck);
    env.add_filter("fromini", fromini);
    env.add_filter("merge", |first: MjValue, rest: Rest<MjValue>| {
        merge_values(std::iter::once(first).chain(rest.iter().cloned()))
    });
    env.add_function("merge", |args: Rest<MjValue>| match args.as_slice() {
        [list] if list.kind() == ValueKind::Seq => merge_values(list.try_iter()?),
        _ => merge_values(args.iter().cloned()),
    });
    env.add_filter("map", map);
    env
}

fn none() -> MjValue {
    MjValue::from(())
}

fn is_blank(v: &MjValue) -> bool {
    v.is_undefined() || v.is_none()
}

/// `p | read`: the file's text. A relative `p` is against `ctx.cwd`, a
/// leading `~` against `ctx.env["HOME"]`. A missing file is `none`.
fn read(ctx: &EngineCtx, path: &MjValue) -> Result<MjValue, Error> {
    if is_blank(path) {
        return Ok(none());
    }
    let Some(p) = path.as_str() else {
        return Err(op_error("read(): the path must be a string"));
    };
    let full = resolve_path(ctx, p);
    match std::fs::read_to_string(&full) {
        Ok(text) => Ok(MjValue::from(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(none()),
        Err(e) => Err(op_error(format!("read(): {}: {e}", full.display()))),
    }
}

/// `p` with a leading `~` replaced by `ctx.env["HOME"]` (when set); any other
/// `p` is returned as is.
pub fn expand_home(ctx: &EngineCtx, p: &str) -> PathBuf {
    match (p.strip_prefix('~'), ctx.env.get("HOME")) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            PathBuf::from(format!("{home}{rest}"))
        }
        _ => PathBuf::from(p),
    }
}

fn resolve_path(ctx: &EngineCtx, p: &str) -> PathBuf {
    let expanded = expand_home(ctx, p);
    match &ctx.cwd {
        Some(cwd) => cwd.join(expanded),
        None => expanded,
    }
}

/// `lup(name, all=false, files=false, dirs=false, follow=true, git=false,
/// root=false)`: liblup's walk from `ctx.cwd`. The nearest path called
/// `name` at or above it, as an absolute string, or `none`; with `all`, a list
/// of every one, nearest first. The walk stops at `ctx.env["HOME"]` when the
/// directory is under it, else at `/`; `git` stops at the repository root
/// (outside one, `none`) and `root` at `/`. `files`/`dirs` match only that
/// kind; `follow=false` matches a symlink itself.
fn lup(ctx: &EngineCtx, name: &str, kwargs: Kwargs) -> Result<MjValue, Error> {
    let flag = |key: &str, default: bool| -> Result<bool, Error> {
        kwargs
            .get::<Option<bool>>(key)
            .map(|v| v.unwrap_or(default))
            .map_err(|_| op_error(format!("lup(): {key}= must be true or false")))
    };
    let all = flag("all", false)?;
    let files = flag("files", false)?;
    let dirs = flag("dirs", false)?;
    let follow = flag("follow", true)?;
    let git = flag("git", false)?;
    let root = flag("root", false)?;
    kwargs.assert_all_used()?;
    if files && dirs {
        return Err(op_error("lup(): files= and dirs= exclude each other"));
    }
    if git && root {
        return Err(op_error("lup(): git= and root= exclude each other"));
    }

    let Some(cwd) = &ctx.cwd else {
        return Ok(none());
    };
    // lup follows `..` on disk, so it starts from the physical directory.
    let Ok(start) = cwd.canonicalize() else {
        return Ok(none());
    };
    let mut query = liblup::Query::new(name.as_bytes());
    query.all = all;
    query.follow = follow;
    query.kind_filter = match (files, dirs) {
        (true, _) => Some(liblup::KindFilter::Files),
        (_, true) => Some(liblup::KindFilter::Dirs),
        _ => None,
    };
    let boundary = if git {
        liblup::Boundary::Git
    } else if root {
        liblup::Boundary::Root
    } else {
        liblup::Boundary::Home(ctx.env.get("HOME").map(String::as_bytes))
    };
    let hits = match liblup::lookup(&query, start.as_os_str().as_bytes(), boundary) {
        Ok(hits) => hits,
        Err(liblup::LupError::NoMatch | liblup::LupError::NotInGitRepo) => return Ok(none()),
        Err(liblup::LupError::UsageError(m)) => return Err(op_error(format!("lup(): {m}"))),
        Err(liblup::LupError::Io(e)) => {
            return Err(op_error(format!("lup(): {}: {e}", start.display())));
        }
    };
    let mut paths = hits.into_iter().filter_map(|hit| match hit {
        liblup::Hit::Path(p) => Some(MjValue::from(String::from_utf8_lossy(&p).into_owned())),
        liblup::Hit::Contents(_) => None,
    });
    Ok(if all {
        MjValue::from(paths.collect::<Vec<_>>())
    } else {
        paths.next().unwrap_or_else(none)
    })
}

/// `p | dirname`: all but the last component; `none` stays `none`.
fn dirname(path: MjValue) -> Result<MjValue, Error> {
    if is_blank(&path) {
        return Ok(none());
    }
    let Some(p) = path.as_str() else {
        return Err(op_error("dirname(): the path must be a string"));
    };
    let trimmed = p.trim_end_matches('/');
    let dir = match trimmed.rsplit_once('/') {
        Some(("", _)) => "/",
        Some((dir, _)) => dir,
        None => "",
    };
    Ok(MjValue::from(dir))
}

enum Step {
    Key(String),
    Index(i64),
}

/// Parse a pluck path: names joined by `.`, `[n]` for a list index, `["k.y"]`
/// for a key holding a dot.
fn parse_path(path: &str) -> Result<Vec<Step>, Error> {
    let bad = || op_error(format!("pluck(): malformed path '{path}'"));
    let mut steps = Vec::new();
    let mut rest = path;
    let mut first = true;
    while !rest.is_empty() {
        if let Some(inner) = rest.strip_prefix('[') {
            let (step, after) =
                if let Some(q) = inner.chars().next().filter(|c| *c == '"' || *c == '\'') {
                    let body = &inner[1..];
                    let end = body.find(q).ok_or_else(bad)?;
                    let after = body[end + 1..].strip_prefix(']').ok_or_else(bad)?;
                    (Step::Key(body[..end].to_string()), after)
                } else {
                    let end = inner.find(']').ok_or_else(bad)?;
                    let n = inner[..end].trim().parse::<i64>().map_err(|_| bad())?;
                    (Step::Index(n), &inner[end + 1..])
                };
            steps.push(step);
            rest = after;
        } else {
            if !first {
                rest = rest.strip_prefix('.').ok_or_else(bad)?;
            }
            let end = rest.find(['.', '[']).unwrap_or(rest.len());
            if end == 0 {
                return Err(bad());
            }
            steps.push(Step::Key(rest[..end].to_string()));
            rest = &rest[end..];
        }
        first = false;
    }
    Ok(steps)
}

/// `v | pluck(path)`: deep get, `none` on any miss.
fn pluck(value: MjValue, path: String) -> Result<MjValue, Error> {
    let mut cur = value;
    for step in parse_path(&path)? {
        cur = match (step, cur.kind()) {
            (Step::Key(k), ValueKind::Map) => cur.get_attr(&k)?,
            (Step::Index(n), ValueKind::Seq) => {
                let len = cur.len().unwrap_or(0) as i64;
                let i = if n < 0 { len + n } else { n };
                if i < 0 || i >= len {
                    return Ok(none());
                }
                cur.get_item_by_index(i as usize)?
            }
            _ => return Ok(none()),
        };
        if is_blank(&cur) {
            return Ok(none());
        }
    }
    Ok(cur)
}

/// `s | fromini`: section -> {key: string}. Keys before any section land
/// under `""`.
fn fromini(text: String) -> Result<MjValue, Error> {
    let ini = ini::Ini::load_from_str(&text).map_err(|e| op_error(format!("fromini: {e}")))?;
    let mut out = serde_json::Map::new();
    for (section, props) in ini.iter() {
        if section.is_none() && props.is_empty() {
            continue;
        }
        let table = out
            .entry(section.unwrap_or("").to_string())
            .or_insert_with(|| serde_json::json!({}));
        for (k, v) in props.iter() {
            table[k] = serde_json::Value::String(v.to_string());
        }
    }
    Ok(MjValue::from_serialize(serde_json::Value::Object(out)))
}

/// Deep merge, later wins; a non-object replaces; a `null` leaf removes.
fn merge_values(values: impl Iterator<Item = MjValue>) -> Result<MjValue, Error> {
    let mut acc = serde_json::Value::Null;
    for v in values {
        let json = serde_json::to_value(&v).map_err(|e| op_error(format!("merge(): {e}")))?;
        acc = merge_json(acc, json);
    }
    Ok(MjValue::from_serialize(acc))
}

fn merge_json(base: serde_json::Value, over: serde_json::Value) -> serde_json::Value {
    use serde_json::Value::Object;
    match (base, over) {
        (Object(mut a), Object(b)) => {
            for (k, v) in b {
                if v.is_null() {
                    a.remove(&k);
                } else {
                    let merged = match a.remove(&k) {
                        Some(old) => merge_json(old, v),
                        None => strip_nulls(v),
                    };
                    a.insert(k, merged);
                }
            }
            Object(a)
        }
        (_, over) => strip_nulls(over),
    }
}

fn strip_nulls(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(m) => serde_json::Value::Object(
            m.into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k, strip_nulls(v)))
                .collect(),
        ),
        v => v,
    }
}

/// `map`, extended to objects: `obj | map('filter', args…)` applies the
/// filter to each value and keeps the keys. Anything else is jinja's `map`.
fn map(state: &State, value: MjValue, args: Rest<MjValue>) -> Result<MjValue, Error> {
    if value.kind() != ValueKind::Map {
        return minijinja::filters::map(state, value, args).map(MjValue::from);
    }
    let (pos, kwargs): (&[MjValue], Kwargs) = minijinja::value::from_args(&args)?;
    let name = match pos.first().and_then(|a| a.as_str()) {
        Some(name) if !kwargs.has("attribute") => name.to_string(),
        _ => return Err(op_error("map over an object takes a filter name")),
    };
    kwargs.assert_all_used()?;
    let mut out = Vec::new();
    for key in value.try_iter()? {
        let item = value.get_item(&key)?;
        let mut call = vec![item];
        call.extend(pos[1..].iter().cloned());
        out.push((key, state.apply_filter(&name, &call)?));
    }
    Ok(MjValue::from_iter(out))
}

/// An `InvalidOperation` error, the one every engine function fails with.
pub fn op_error(message: impl Into<std::borrow::Cow<'static, str>>) -> Error {
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

    /// The expression's value as compact JSON (keys sorted).
    fn eval_json(expr: &str) -> String {
        let env = build_engine_env(ctx_with(&[], None));
        let value = env
            .compile_expression(expr)
            .unwrap()
            .eval(MjValue::from_serialize(serde_json::json!({})))
            .unwrap();
        serde_json::to_string(&value).unwrap()
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

    /// A tempdir tree `<root>/a/.git/HEAD` and `<root>/a/b/c`, with the ctx
    /// rooted at `<root>/a/b/c` and `HOME` set to `<root>`.
    fn repo() -> (tempfile::TempDir, std::path::PathBuf, Arc<EngineCtx>) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("a/.git")).unwrap();
        std::fs::write(root.join("a/.git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::create_dir_all(root.join("a/b/c")).unwrap();
        let cwd = root.join("a/b/c");
        let ctx = ctx_with(&[("HOME", root.to_str().unwrap())], Some(&cwd));
        (dir, root, ctx)
    }

    #[test]
    fn read_relative_to_cwd() {
        let (_d, root, ctx) = repo();
        std::fs::write(root.join("a/b/c/note.txt"), "hello\n").unwrap();
        assert_eq!(eval_in(ctx, "'note.txt' | read").unwrap(), "hello\n");
    }

    #[test]
    fn read_is_a_filter_only() {
        let (_d, root, ctx) = repo();
        std::fs::write(root.join("a/b/c/note.txt"), "hello\n").unwrap();
        let err = eval_in(ctx, "read('note.txt')").unwrap_err();
        assert!(err.contains("unknown function"), "unexpected error: {err}");
    }

    #[test]
    fn read_tilde_uses_env_home() {
        let (_d, root, ctx) = repo();
        std::fs::write(root.join("home.txt"), "h").unwrap();
        assert_eq!(eval_in(ctx, "'~/home.txt' | read").unwrap(), "h");
    }

    #[test]
    fn read_missing_is_none() {
        let (_d, _root, ctx) = repo();
        assert_eq!(
            eval_in(ctx.clone(), "('nope' | read) is none").unwrap(),
            "true"
        );
        assert_eq!(
            eval_in(ctx, "('nope' | read) or 'fallback'").unwrap(),
            "fallback"
        );
    }

    #[test]
    fn read_non_utf8_is_an_error_naming_the_path() {
        let (_d, root, ctx) = repo();
        std::fs::write(root.join("a/b/c/bin"), [0xff, 0xfe]).unwrap();
        let err = eval_in(ctx, "'bin' | read").unwrap_err();
        assert!(err.contains("bin"), "unexpected error: {err}");
    }

    #[test]
    fn read_other_io_error_is_an_error_naming_the_path() {
        let (_d, _root, ctx) = repo();
        let err = eval_in(ctx, "'.' | read").unwrap_err();
        assert!(err.contains("read():") && err.contains('.'), "{err}");
    }

    #[test]
    fn lup_finds_the_nearest_path_at_or_above_cwd() {
        let (_d, root, ctx) = repo();
        let want = root.join("a/.git");
        assert_eq!(eval_in(ctx, "lup('.git')").unwrap(), want.to_str().unwrap());
    }

    #[test]
    fn lup_miss_is_none() {
        let (_d, _root, ctx) = repo();
        assert_eq!(
            eval_in(ctx, "lup('.nothing-here') is none").unwrap(),
            "true"
        );
        assert_eq!(eval("lup('.git') is none").unwrap(), "true");
    }

    #[test]
    fn lup_stops_at_home() {
        let (_d, root, _ctx) = repo();
        // HOME is <root>/a/b: the .git in <root>/a is above the boundary.
        let cwd = root.join("a/b/c");
        let home = root.join("a/b");
        let ctx = ctx_with(&[("HOME", home.to_str().unwrap())], Some(&cwd));
        assert_eq!(eval_in(ctx, "lup('.git') is none").unwrap(), "true");
    }

    #[test]
    fn lup_all_lists_every_hit_nearest_first() {
        let (_d, root, ctx) = repo();
        std::fs::write(root.join("a/b/.env"), "").unwrap();
        std::fs::write(root.join(".env"), "").unwrap();
        let want = format!(
            "[{:?}, {:?}]",
            root.join("a/b/.env").to_str().unwrap(),
            root.join(".env").to_str().unwrap()
        );
        assert_eq!(eval_in(ctx, "lup('.env', all=true)").unwrap(), want);
    }

    #[test]
    fn lup_dirs_skips_a_nearer_file() {
        let (_d, root, ctx) = repo();
        std::fs::write(root.join("a/b/.git"), "gitdir: elsewhere\n").unwrap();
        assert_eq!(
            eval_in(ctx.clone(), "lup('.git')").unwrap(),
            root.join("a/b/.git").to_str().unwrap()
        );
        assert_eq!(
            eval_in(ctx, "lup('.git', dirs=true)").unwrap(),
            root.join("a/.git").to_str().unwrap()
        );
    }

    #[test]
    fn lup_git_stops_at_the_repository_root() {
        let (_d, root, ctx) = repo();
        std::fs::write(root.join(".env"), "").unwrap();
        assert_eq!(
            eval_in(ctx.clone(), "lup('.env')").unwrap(),
            root.join(".env").to_str().unwrap()
        );
        assert_eq!(
            eval_in(ctx, "lup('.env', git=true) is none").unwrap(),
            "true"
        );
    }

    #[test]
    fn lup_git_outside_a_repository_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_with(&[], Some(dir.path()));
        assert_eq!(
            eval_in(ctx, "lup('.env', git=true) is none").unwrap(),
            "true"
        );
    }

    #[test]
    fn lup_walks_from_the_physical_directory() {
        let (_d, root, _ctx) = repo();
        let link = root.join("link");
        std::os::unix::fs::symlink(root.join("a/b/c"), &link).unwrap();
        let ctx = ctx_with(&[("HOME", root.to_str().unwrap())], Some(&link));
        assert_eq!(
            eval_in(ctx, "lup('.git')").unwrap(),
            root.join("a/.git").to_str().unwrap()
        );
    }

    #[test]
    fn lup_exclusive_options_are_errors() {
        let (_d, _root, ctx) = repo();
        let err = eval_in(ctx.clone(), "lup('.git', files=true, dirs=true)").unwrap_err();
        assert!(err.contains("files= and dirs="), "unexpected error: {err}");
        let err = eval_in(ctx.clone(), "lup('.git', git=true, root=true)").unwrap_err();
        assert!(err.contains("git= and root="), "unexpected error: {err}");
        let err = eval_in(ctx, "lup('.git', all='yes')").unwrap_err();
        assert!(err.contains("all= must be"), "unexpected error: {err}");
    }

    #[test]
    fn root_line_from_the_spec() {
        let (_d, root, ctx) = repo();
        assert_eq!(
            eval_in(ctx, "lup('.git') | dirname").unwrap(),
            root.join("a").to_str().unwrap()
        );
    }

    #[test]
    fn head_line_from_the_spec() {
        let (_d, _root, ctx) = repo();
        assert_eq!(
            eval_in(ctx, "lup('.git/HEAD') | read | trim").unwrap(),
            "ref: refs/heads/main"
        );
    }

    #[test]
    fn dirname_forms() {
        assert_eq!(eval("'/a/b/c.txt' | dirname").unwrap(), "/a/b");
        assert_eq!(eval("'c.txt' | dirname").unwrap(), "");
        assert_eq!(eval("'/c' | dirname").unwrap(), "/");
        assert_eq!(eval("'/a/b/' | dirname").unwrap(), "/a");
        assert_eq!(eval("(none | dirname) is none").unwrap(), "true");
    }

    #[test]
    fn pluck_path_forms() {
        let data = r#"'{"node":[{"version":"22.1.0"},{"version":"20.0.0"}],"k.y":7,"a":{"b":{"c":1}}}' | fromjson"#;
        let p = |path: &str| eval(&format!("({data}) | pluck('{path}')")).unwrap();
        assert_eq!(p("node[0].version"), "22.1.0");
        assert_eq!(p("node[-1].version"), "20.0.0");
        assert_eq!(p("a.b.c"), "1");
        assert_eq!(p(r#"[\"k.y\"]"#), "7");
        assert_eq!(p("a.missing.c"), "none");
        assert_eq!(p("node[5]"), "none");
        assert_eq!(p("a[0]"), "none");
        assert_eq!(p("node.version"), "none");
        let err = eval(&format!("({data}) | pluck('a[')")).unwrap_err();
        assert!(err.contains("pluck()"), "unexpected error: {err}");
    }

    #[test]
    fn map_over_an_object_keeps_keys() {
        let mise = r#"'{"node":[{"version":"22.1.0"}]}' | fromjson"#;
        assert_eq!(
            eval_json(&format!("({mise}) | map('pluck', '[0].version')")),
            r#"{"node":"22.1.0"}"#
        );
    }

    #[test]
    fn map_over_an_object_with_attribute_is_an_error() {
        let err = eval(r#"('{"a":1}' | fromjson) | map(attribute='x')"#).unwrap_err();
        assert!(
            err.contains("map over an object takes a filter name"),
            "{err}"
        );
    }

    #[test]
    fn map_over_a_list_is_jinjas_map() {
        assert_eq!(
            eval("['a', 'b'] | map('upper') | join(',')").unwrap(),
            "A,B"
        );
        assert_eq!(
            eval(r#"[{"v":1},{"v":2}] | map(attribute='v') | join(',')"#).unwrap(),
            "1,2"
        );
    }

    #[test]
    fn fromini_sections() {
        let ini = "top=1\n[core]\nname = x\nflag=yes\n[profile dev]\nregion=eu\n";
        let out = eval_json(&format!("'{}' | fromini", ini.replace('\n', "\\n")));
        assert_eq!(
            out,
            r#"{"":{"top":"1"},"core":{"flag":"yes","name":"x"},"profile dev":{"region":"eu"}}"#
        );
        assert_eq!(eval_json("'[s]\\nk=v' | fromini"), r#"{"s":{"k":"v"}}"#);
    }

    #[test]
    fn merge_later_wins_and_none_removes() {
        let a = r#"{"x":{"p":1,"q":2},"y":1,"z":[1]}"#;
        let b = r#"{"x":{"q":3,"r":4},"y":{"n":1},"z":[2]}"#;
        let j = eval_json;
        let want = r#"{"x":{"p":1,"q":3,"r":4},"y":{"n":1},"z":[2]}"#;
        assert_eq!(j(&format!("({a}) | merge({b})")), want);
        assert_eq!(j(&format!("merge({a}, {b})")), want);
        assert_eq!(j(&format!("merge([{a}, {b}])")), want);
        assert_eq!(
            j(&format!("({a}) | merge({{'x': {{'p': none}}, 'y': none}})")),
            r#"{"x":{"q":2},"z":[1]}"#
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
