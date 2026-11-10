//! `log.debug/info/warn/error/fatal(msg)` and `fail(msg)`: a line a template
//! writes, and the way it fails a render on purpose.
//!
//! `debug`, `info` and `warn` emit a `tracing` event and the render goes on.
//! `error` and `fatal` fail the render through [`op_error`], so they take the
//! path a failed `exec()` takes. `fail(msg)` is `log.error(msg)`.

use crate::engine::{ctx, op_error};
use minijinja::value::{Object, Value as MjValue};
use minijinja::{Environment, Error, State};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

/// The tracing target of every line a template logs.
pub const TARGET: &str = "beachcomber::template";

const METHODS: &str = "debug, info, warn, error, fatal";

/// What `log.fatal` fails a render with: an error carrying this as its
/// source, found by [`fatal_in`].
// shortcut: fatal fails exactly as error; the scheduler does not yet stop retrying after it.
#[derive(Debug)]
pub struct Fatal;

impl std::fmt::Display for Fatal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("fatal")
    }
}

impl std::error::Error for Fatal {}

/// Whether `e` is a `log.fatal`, anywhere down its sources.
pub fn fatal_in(e: &Error) -> bool {
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(c) = cause {
        if c.is::<Fatal>() {
            return true;
        }
        cause = c.source();
    }
    false
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Level {
    Debug,
    Info,
    Warn,
}

fn emit(level: Level, origin: Option<&str>, msg: &str) {
    let origin = origin.unwrap_or("");
    match level {
        Level::Debug => tracing::debug!(target: "beachcomber::template", origin, "{msg}"),
        Level::Info => tracing::info!(target: "beachcomber::template", origin, "{msg}"),
        Level::Warn => tracing::warn!(target: "beachcomber::template", origin, "{msg}"),
    }
}

/// Per origin, the last line logged and how many times it came again.
static LAST: LazyLock<Mutex<HashMap<String, Last>>> = LazyLock::new(Mutex::default);

/// A line's level, its text and how many times it came again.
type Last = (Level, String, u64);

/// Log `msg`, once while an origin repeats it: a repeat is counted, and the
/// count is logged when a different line follows. No origin, no counting.
fn write(level: Level, msg: &str) {
    let ctx = ctx();
    let origin = ctx.origin();
    let Some(key) = origin.as_deref() else {
        return emit(level, None, msg);
    };
    let mut last = LAST.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((l, m, n)) = last.get_mut(key) {
        if *l == level && m == msg {
            *n += 1;
            return;
        }
        if *n > 0 {
            emit(*l, Some(key), &format!("{m} (repeated {n} times)"));
        }
    }
    last.insert(key.to_string(), (level, msg.to_string(), 0));
    drop(last);
    emit(level, Some(key), msg);
}

fn text(args: &[MjValue], what: &str) -> Result<String, Error> {
    match args {
        [m] => Ok(m.as_str().map_or_else(|| m.to_string(), str::to_string)),
        _ => Err(op_error(format!("{what}(): one message"))),
    }
}

fn fail_with(msg: String) -> Error {
    op_error(msg)
}

#[derive(Debug)]
struct Log;

impl Object for Log {
    fn call_method(
        self: &Arc<Self>,
        _state: &mut State<'_, '_>,
        method: &str,
        args: &[MjValue],
    ) -> Result<MjValue, Error> {
        let level = match method {
            "debug" => Level::Debug,
            "info" => Level::Info,
            "warn" => Level::Warn,
            "error" => return Err(fail_with(text(args, "log.error")?)),
            "fatal" => return Err(fail_with(text(args, "log.fatal")?).with_source(Fatal)),
            _ => {
                return Err(op_error(format!("log.{method}(): log has {METHODS}")));
            }
        };
        write(level, &text(args, &format!("log.{method}"))?);
        Ok(MjValue::from(""))
    }
}

pub(super) fn add_log(env: &mut Environment<'_>) {
    env.add_global("log", MjValue::from_object(Log));
}

pub(super) fn add_fail(env: &mut Environment<'_>) {
    env.add_function("fail", |args: minijinja::value::Rest<MjValue>| {
        Err::<MjValue, _>(fail_with(text(&args, "fail")?))
    });
}

#[cfg(test)]
mod tests {
    use super::super::testing::{ctx_with, eval_in};
    use super::*;
    use std::io::Write;

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// `exprs` rendered in order with `origin`; what tracing saw, one line each.
    fn run(origin: Option<&str>, exprs: &[&str]) -> Vec<String> {
        let buf = Buf::default();
        let sink = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || sink.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .without_time()
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            for e in exprs {
                let mut c = ctx_with(&[], None);
                Arc::get_mut(&mut c)
                    .unwrap()
                    .set_origin(origin.map(Into::into));
                assert_eq!(eval_in(c, e).as_deref(), Ok(""), "{e}");
            }
        });
        let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        out.lines().map(str::to_string).collect()
    }

    #[test]
    fn each_level_renders_nothing_and_logs_at_its_level_with_the_origin() {
        for (m, lvl) in [("debug", "DEBUG"), ("info", "INFO"), ("warn", "WARN")] {
            let origin = format!("lvl.{m}");
            let lines = run(Some(&origin), &[&format!("log.{m}('hi')")]);
            assert_eq!(lines.len(), 1, "{lines:?}");
            assert!(lines[0].contains(lvl), "{lines:?}");
            assert!(
                lines[0].contains(&origin) && lines[0].contains("hi"),
                "{lines:?}"
            );
            assert!(lines[0].contains(TARGET), "{lines:?}");
        }
    }

    #[test]
    fn a_non_string_is_stringified() {
        let lines = run(Some("t.nonstr"), &["log.info(42)"]);
        assert!(lines[0].contains("42"), "{lines:?}");
    }

    #[test]
    fn error_and_fail_fail_with_the_message() {
        for e in ["log.error('no ssh agent')", "fail('no ssh agent')"] {
            let err = eval_in(ctx_with(&[], None), e).unwrap_err();
            assert_eq!(
                crate::eval::without_engine_frame(&err),
                "no ssh agent",
                "{e}"
            );
        }
    }

    #[test]
    fn fatal_fails_and_is_marked() {
        let env = crate::engine::engine_env();
        let marked = |src: &str| {
            let e = env.compile_expression(src).unwrap().eval(()).unwrap_err();
            (fatal_in(&e), e.to_string())
        };
        let (fatal, msg) = marked("log.fatal('gone')");
        assert!(fatal && msg.contains("gone"), "{msg}");
        assert!(!marked("log.error('gone')").0);
        assert!(!marked("fail('gone')").0);
    }

    #[test]
    fn an_unknown_method_lists_the_five() {
        for m in ["critical", "trace"] {
            let err = eval_in(ctx_with(&[], None), &format!("log.{m}('x')")).unwrap_err();
            assert!(err.contains(METHODS), "{err}");
        }
    }

    #[test]
    fn a_repeated_line_logs_once_then_counts() {
        let lines = run(
            Some("t.dedupe"),
            &[
                "log.warn('a')",
                "log.warn('a')",
                "log.warn('a')",
                "log.warn('b')",
            ],
        );
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(
            lines[0].ends_with(" a") || lines[0].contains(": a"),
            "{lines:?}"
        );
        assert!(
            lines[1].contains("a (repeated 2 times)") && lines[1].contains("WARN"),
            "{lines:?}"
        );
        assert!(
            lines[2].contains(": b") || lines[2].ends_with(" b"),
            "{lines:?}"
        );
    }

    #[test]
    fn no_origin_is_not_deduplicated() {
        assert_eq!(run(None, &["log.warn('z')", "log.warn('z')"]).len(), 2);
    }
}
