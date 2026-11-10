//! `log.debug/info/warn/error/fatal(msg)` and `_.fail(msg)`: a line a template
//! writes, and the way it fails a render on purpose.
//!
//! `debug`, `info` and `warn` emit a `tracing` event and the render goes on.
//! `error` and `fatal` fail the render through [`op_error`], so they take the
//! path a failed `_.exec()` takes. `_.fail(msg)` is `_.log.error(msg)`.

use crate::engine::{ctx, op_error};
use crate::protocol::{Level as Wire, LogLine};
use minijinja::value::{Object, Value as MjValue};
use minijinja::{Environment, Error, State};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

/// The tracing target of every line a template logs.
pub const TARGET: &str = "beachcomber::template";

const METHODS: &str = "debug, info, warn, error, fatal";

/// What `log.fatal` fails a render with: an error carrying this as its
/// source, found by [`fatal_in`]. A Source's run that fails with it is stopped
/// until something re-arms it (a changed input, a reload, a refresh).
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

impl Level {
    fn wire(self) -> Wire {
        match self {
            Level::Debug => Wire::Debug,
            Level::Info => Wire::Info,
            Level::Warn => Wire::Warn,
        }
    }
}

/// The most lines a log keeps ([`merge`]); one more says how many it left out.
pub const MAX_LINES: usize = 16;
/// The most bytes of one line's text a log keeps, as an `exec`'s stderr tail.
pub const MAX_BYTES: usize = 512;
/// The text of the line that counts the lines a full log left out.
pub const MORE: &str = "… more lines";

/// The lines this thread is collecting ([`capture`]): the least level kept,
/// the origin each is given, whether each is given the leaf its render is
/// for (a client's), and the lines so far.
struct Capture {
    min: Wire,
    origin: String,
    keyed: bool,
    lines: Vec<LogLine>,
}

thread_local! {
    static CAPTURE: RefCell<Option<Capture>> = const { RefCell::new(None) };
}

/// Run `f`, collecting the lines at `min` and above that it raises on this
/// thread ([`raise`], and every `_.log.debug/info/warn`), each given `origin`
/// and, when `keyed`, the leaf the render in progress is for (its context's
/// origin). A capture inside another has the thread to itself until it ends.
pub fn capture<T>(
    min: Wire,
    origin: &str,
    keyed: bool,
    f: impl FnOnce() -> T,
) -> (T, Vec<LogLine>) {
    /// Puts the outer capture back should `f` panic.
    struct Restore(Option<Option<Capture>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(outer) = self.0.take() {
                CAPTURE.with(|c| c.replace(outer));
            }
        }
    }
    let outer = CAPTURE.with(|c| {
        c.replace(Some(Capture {
            min,
            origin: origin.to_string(),
            keyed,
            lines: Vec::new(),
        }))
    });
    let mut restore = Restore(Some(outer));
    let out = f();
    let outer = restore.0.take().flatten();
    let lines = CAPTURE
        .with(|c| c.replace(outer))
        .map(|c| c.lines)
        .unwrap_or_default();
    (out, lines)
}

/// The least level the capture on this thread keeps, its origin and whether
/// it is keyed; `None` with no capture. What a render handed to another
/// thread captures there with ([`capture`]), to [`extend`] this one after.
pub fn capturing() -> Option<(Wire, String, bool)> {
    CAPTURE.with(|c| {
        c.borrow()
            .as_ref()
            .map(|cap| (cap.min, cap.origin.clone(), cap.keyed))
    })
}

/// Whether the capture on this thread keeps a line at `level`: what a line
/// costly to word is asked before it is worded.
pub fn wanted(level: Wire) -> bool {
    CAPTURE.with(|c| c.borrow().as_ref().is_some_and(|cap| level >= cap.min))
}

/// Add `lines`, made elsewhere, to the capture on this thread, those at its
/// level and above.
pub fn extend(lines: Vec<LogLine>) {
    CAPTURE.with(|c| {
        if let Some(cap) = c.borrow_mut().as_mut() {
            for line in lines.into_iter().filter(|l| l.level >= cap.min) {
                push(&mut cap.lines, line);
            }
        }
    });
}

/// A line at `level` for the capture on this thread, if any wants it.
pub fn raise(level: Wire, msg: &str) {
    raise_at(level, None, None, msg);
}

/// [`raise`], from `origin` rather than the capture's, about the leaf `key`
/// rather than the one the render in progress is for.
pub fn raise_at(level: Wire, origin: Option<&str>, key: Option<&str>, msg: &str) {
    CAPTURE.with(|c| {
        if let Some(cap) = c.borrow_mut().as_mut()
            && level >= cap.min
        {
            let key = match key {
                Some(key) => Some(key.to_string()),
                None if cap.keyed => ctx().origin(),
                None => None,
            };
            let mut line = LogLine::new(level, origin.unwrap_or(&cap.origin), msg);
            line.key = key;
            push(&mut cap.lines, line);
        }
    });
}

/// Add `lines` to `log`, within its bounds: a line already there (same level,
/// text, origin, path and key) is counted, not repeated; a text is cut to [`MAX_BYTES`];
/// past [`MAX_LINES`] one line, [`MORE`], counts the rest at the highest
/// level among them.
pub fn merge(log: &mut Vec<LogLine>, lines: Vec<LogLine>) {
    for line in lines {
        push(log, line);
    }
}

fn push(log: &mut Vec<LogLine>, mut line: LogLine) {
    if line.msg.len() > MAX_BYTES {
        let mut end = MAX_BYTES;
        while !line.msg.is_char_boundary(end) {
            end -= 1;
        }
        line.msg.truncate(end);
        line.msg.push('…');
    }
    let n = line.count.unwrap_or(1);
    if let Some(same) = log.iter_mut().find(|l| {
        l.level == line.level
            && l.msg == line.msg
            && l.origin == line.origin
            && l.path == line.path
            && l.key == line.key
    }) {
        same.count = Some(same.count.unwrap_or(1) + n);
        return;
    }
    if log.len() < MAX_LINES {
        log.push(line);
        return;
    }
    match log.get_mut(MAX_LINES) {
        Some(more) => {
            more.count = Some(more.count.unwrap_or(1) + n);
            more.level = more.level.max(line.level);
        }
        None => log.push(LogLine {
            msg: MORE.to_string(),
            count: Some(n),
            ..line
        }),
    }
}

/// Per origin, the last line logged and how many times it came again.
static LAST: LazyLock<Mutex<HashMap<String, Last>>> = LazyLock::new(Mutex::default);

/// A line's level, its text and how many times it came again.
type Last = (Level, String, u64);

/// Log `msg`, once while an origin repeats it: a repeat is counted, and the
/// count is logged when a different line follows. No origin, no counting.
fn write(level: Level, msg: &str) {
    raise(level.wire(), msg);
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
            let lines = run(Some(&origin), &[&format!("_.log.{m}('hi')")]);
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
        let lines = run(Some("t.nonstr"), &["_.log.info(42)"]);
        assert!(lines[0].contains("42"), "{lines:?}");
    }

    #[test]
    fn error_and_fail_fail_with_the_message() {
        for e in ["_.log.error('no ssh agent')", "_.fail('no ssh agent')"] {
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
        let (fatal, msg) = marked("_.log.fatal('gone')");
        assert!(fatal && msg.contains("gone"), "{msg}");
        assert!(!marked("_.log.error('gone')").0);
        assert!(!marked("_.fail('gone')").0);
    }

    #[test]
    fn an_unknown_method_lists_the_five() {
        for m in ["critical", "trace"] {
            let err = eval_in(ctx_with(&[], None), &format!("_.log.{m}('x')")).unwrap_err();
            assert!(err.contains(METHODS), "{err}");
        }
    }

    #[test]
    fn a_repeated_line_logs_once_then_counts() {
        let lines = run(
            Some("t.dedupe"),
            &[
                "_.log.warn('a')",
                "_.log.warn('a')",
                "_.log.warn('a')",
                "_.log.warn('b')",
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
        assert_eq!(run(None, &["_.log.warn('z')", "_.log.warn('z')"]).len(), 2);
    }

    fn line(level: Wire, msg: &str) -> LogLine {
        LogLine::new(level, "o", msg)
    }

    #[test]
    fn a_capture_keeps_its_levels_and_gives_the_thread_back() {
        let ((), outer) = capture(Wire::Info, "outer", false, || {
            raise(Wire::Debug, "dropped");
            raise(Wire::Info, "a");
            let ((), inner) = capture(Wire::Debug, "inner", false, || raise(Wire::Debug, "d"));
            assert_eq!(inner.len(), 1);
            assert_eq!(inner[0].origin, "inner");
            raise(Wire::Warn, "a");
        });
        let shown: Vec<_> = outer.iter().map(|l| (l.level, l.msg.as_str())).collect();
        assert_eq!(shown, [(Wire::Info, "a"), (Wire::Warn, "a")]);
        raise(Wire::Warn, "nobody listens");
    }

    #[test]
    fn a_log_counts_repeats_cuts_long_lines_and_stops_at_its_bound() {
        let mut log = Vec::new();
        merge(&mut log, vec![line(Wire::Warn, "x"), line(Wire::Warn, "x")]);
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].count, Some(2));
        merge(&mut log, vec![line(Wire::Info, &"é".repeat(MAX_BYTES))]);
        assert!(log[1].msg.len() <= MAX_BYTES + '…'.len_utf8());
        let many = (0..20).map(|i| line(Wire::Info, &i.to_string())).collect();
        merge(&mut log, many);
        merge(&mut log, vec![line(Wire::Error, "late")]);
        assert_eq!(log.len(), MAX_LINES + 1);
        let more = &log[MAX_LINES];
        assert_eq!(
            (more.msg.as_str(), more.count, more.level),
            (MORE, Some(7), Wire::Error)
        );
    }
}
