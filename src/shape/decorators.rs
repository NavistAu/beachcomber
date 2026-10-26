//! Return decorators (Decisions 7 and 8): a Source's schedule, recorded.
//!
//! A Source's `return` line is rendered once, at load, with `result` bound to
//! a sentinel and `poll`, `watch`, `ttl`, `failback`, `budget`, `timeout`, `nearest` and
//! `super`
//! registered as filters that record their call and hand their input on. The
//! render must give the sentinel back. The context holds nothing else (`_`
//! is `{"name": ...}`), undefined is an error, and the environment is the
//! pure shared filters only (no `exec`, `read`, `url`, ...), so an argument
//! that names anything but a literal, `_.name` or an expression over those
//! cannot render.

use crate::provider::{FailbackConfig, InvalidationStrategy, expand_abs_path};
use crate::scheduler::lifecycle::DEFAULT_POLL_COUNT;
use libbeachcomber::engine::build_pure_engine_env;
use libbeachcomber::engine::op_error;
use minijinja::value::{Kwargs, Object, ObjectRepr, Rest, Value as MjValue, ValueKind};
use minijinja::{Environment, Error, ErrorKind, UndefinedBehavior};
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What the sentinel prints as, and so what a render of `result` alone is.
const SENTINEL: &str = "@@result-sentinel@@";
/// A year; no interval is longer.
const MAX_INTERVAL_SECS: u64 = 365 * 24 * 3600;
/// Counts multiply an interval into a lifespan, which decay then doubles.
const MAX_COUNT: u32 = 10_000;
const MUST_DECORATE: &str = "a Source's return must decorate result";
const DECORATORS: [&str; 7] = [
    "poll", "watch", "ttl", "failback", "budget", "timeout", "nearest",
];

/// What `result` is bound to: an opaque object. It has no length, items or
/// attributes, so anything that needs them errors; and printing it counts, so
/// a decorator whose arguments were built by stringifying it (`'x' ~ result`,
/// `result | upper`, `result | truncate(5)`) is told apart from one handed the
/// plain literal. A line's own output is the only legitimate print, and that
/// happens after its last decorator.
struct Sentinel {
    printed: Arc<AtomicUsize>,
}

impl fmt::Debug for Sentinel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(SENTINEL)
    }
}

impl fmt::Display for Sentinel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.printed.fetch_add(1, Ordering::SeqCst);
        f.write_str(SENTINEL)
    }
}

impl Object for Sentinel {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Plain
    }

    fn render(self: &Arc<Self>, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// What a Source's return chain says about its schedule, with the engine's
/// defaults filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub invalidation: InvalidationStrategy,
    pub failback: FailbackConfig,
    pub budget: Duration,
    /// How long one run may take before it is cut off.
    pub timeout: Duration,
    /// `Some(1)` for a `ttl(...)` Source; otherwise the provider's `_.decay`
    /// decides.
    pub decay: Option<u8>,
    /// `| nearest`: the nearest instance answers alone. Not saying it is
    /// cascade.
    pub nearest: bool,
}

/// What a Source gets for a decorator its chain does not name: every Source
/// has a `failback`, a `budget` and a `timeout`, with no off switch.
#[derive(Debug, Clone, Copy)]
pub struct EngineDefaults {
    pub failback: FailbackConfig,
    pub budget: Duration,
    pub timeout: Duration,
}

/// The decorators one line recorded, each the last call of its name.
#[derive(Default)]
struct Line {
    poll: Option<(u64, u32)>,
    watch: Option<(Vec<String>, Vec<String>)>,
    ttl: Option<u64>,
    failback: Option<FailbackConfig>,
    budget: Option<Duration>,
    timeout: Option<Duration>,
    nearest: bool,
}

struct Render {
    /// The engine each line renders in a copy of, its decorators bound to
    /// this render ([`return_env`]).
    base: Environment<'static>,
    lines: Vec<String>,
    name: String,
    /// The variables a path argument (`watch('$XDG_CONFIG_HOME/x')`) expands from.
    vars: HashMap<String, String>,
    recorded: Mutex<Vec<Line>>,
    /// The first failure, and the level whose line raised it.
    failure: Mutex<Option<ReturnError>>,
}

impl Render {
    /// Record `message` as the failure of `level` (the first one wins) and
    /// make the error that stops the render.
    fn fail(&self, level: usize, message: impl Into<String>) -> Error {
        let message = message.into();
        let mut failure = self.failure.lock().unwrap_or_else(|e| e.into_inner());
        failure.get_or_insert_with(|| ReturnError {
            level,
            message: message.clone(),
        });
        op_error(message)
    }

    fn has_failed(&self) -> bool {
        self.failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }
}

/// Why a return chain does not load, and which line of it said so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnError {
    /// Index into the `lines` given to [`record_return`]: 0 is the own line,
    /// 1 the next one up, which `| super` reached.
    pub level: usize,
    pub message: String,
}

impl fmt::Display for ReturnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ReturnError {}

/// Render the chain `lines` (every value of the `return` key along the
/// chain, nearest first: `[own, <p>.sources._, _.sources._]`, or just the
/// `_`s when the own line is absent) for the Source `name`, and read its
/// schedule off the recorded calls. A line's `| super` renders the next one;
/// a line that never calls it leaves the rest unused.
///
/// A later call of a decorator in one line replaces an earlier one, whole.
/// Across `| super` a Source's own setting beats its prototype's: a decorator
/// a nearer line set is kept and the farther line's call of it is dropped.
/// `ttl` is `poll` with a count of 1 and a decay of 1: the two share one slot.
///
/// `vars` are the variables a path argument expands from: the only part of a
/// decorator's arguments that reads anything outside the line (the rest are
/// literals and `_.name`). `env` is the engine the lines render in, a load's
/// one [`return_env`].
pub fn record_return(
    lines: &[&str],
    name: &str,
    defaults: &EngineDefaults,
    vars: &HashMap<String, String>,
    env: &Environment<'static>,
) -> Result<Recorded, ReturnError> {
    if lines.is_empty() {
        return Err(ReturnError {
            level: 0,
            message: "a Source needs a return line".into(),
        });
    }
    // A statement no Source line may hold (an import, an `include`) is
    // refused as written, before anything renders.
    for (level, line) in lines.iter().enumerate() {
        if let Some(finding) =
            libbeachcomber::eval::refused(line, libbeachcomber::eval::Line::Return)
                .into_iter()
                .next()
        {
            return Err(ReturnError {
                level,
                message: finding.to_string(),
            });
        }
    }
    let render = Arc::new(Render {
        base: env.clone(),
        lines: lines.iter().map(|l| l.to_string()).collect(),
        name: name.to_string(),
        vars: vars.clone(),
        recorded: Mutex::new(lines.iter().map(|_| Line::default()).collect()),
        failure: Mutex::new(None),
    });
    if let Err(error) = render_level(&render, 0) {
        let failure = render.failure.lock().unwrap_or_else(|e| e.into_inner());
        return Err(failure.clone().unwrap_or_else(|| ReturnError {
            level: 0,
            message: error.to_string(),
        }));
    }
    let recorded = render.recorded.lock().unwrap_or_else(|e| e.into_inner());
    merge(&recorded, defaults)
}

/// The nearest line's call of each decorator.
fn merge(levels: &[Line], defaults: &EngineDefaults) -> Result<Recorded, ReturnError> {
    if let Some(level) = levels
        .iter()
        .position(|l| l.poll.is_some() && l.ttl.is_some())
    {
        return Err(ReturnError {
            level,
            message: "ttl() and poll() cannot be combined".into(),
        });
    }
    // `ttl` is sugar for `poll(n, 1)` plus decay 1: one slot, the nearest
    // line's call winning.
    let (schedule, decay) = match levels
        .iter()
        .find(|l| l.poll.is_some() || l.ttl.is_some())
        .map(|l| (l.ttl, l.poll))
    {
        Some((Some(secs), _)) => (Some((secs, 1)), Some(1)),
        Some((None, poll)) => (poll, None),
        None => (None, None),
    };
    let watch = levels.iter().find_map(|l| l.watch.clone());
    let invalidation = match (watch, schedule) {
        (None, Some((interval_secs, count))) => InvalidationStrategy::Poll {
            interval_secs,
            count,
        },
        (Some((patterns, abs_paths)), None) => InvalidationStrategy::Watch {
            patterns,
            abs_paths,
        },
        (Some((patterns, abs_paths)), Some((interval_secs, count))) => {
            InvalidationStrategy::WatchAndPoll {
                patterns,
                abs_paths,
                interval_secs,
                count,
            }
        }
        (None, None) => InvalidationStrategy::Watch {
            patterns: vec![],
            abs_paths: vec![],
        },
    };
    Ok(Recorded {
        invalidation,
        failback: levels
            .iter()
            .find_map(|l| l.failback)
            .unwrap_or(defaults.failback),
        budget: levels
            .iter()
            .find_map(|l| l.budget)
            .unwrap_or(defaults.budget),
        timeout: levels
            .iter()
            .find_map(|l| l.timeout)
            .unwrap_or(defaults.timeout),
        decay,
        nearest: levels.iter().any(|l| l.nearest),
    })
}

/// The engine a `return` line renders in: the pure library, an undefined
/// name an error, and no decorator yet. A render copies it and binds its
/// decorators to the copy; copying shares what it holds, so one is built per
/// load ([`crate::shape::build`]) and handed to every [`record_return`].
pub fn return_env() -> Environment<'static> {
    let mut jinja = build_pure_engine_env(Default::default());
    jinja.set_undefined_behavior(UndefinedBehavior::Strict);
    jinja
}

/// A line's context: the sentinel (counting its prints in `printed`) and `_`.
fn context(render: &Render, printed: &Arc<AtomicUsize>) -> MjValue {
    let sentinel = MjValue::from_object(Sentinel {
        printed: printed.clone(),
    });
    let underscore = MjValue::from_serialize(serde_json::json!({"name": render.name}));
    MjValue::from_iter([("result", sentinel), ("_", underscore)])
}

/// Render `render.lines[level]`, its `super` rendering the next line. Every
/// failure leaves its message and level in `render.failure`.
fn render_level(render: &Arc<Render>, level: usize) -> Result<(), Error> {
    let printed = Arc::new(AtomicUsize::new(0));
    let mut jinja = render.base.clone();
    for decorator in DECORATORS {
        let (render, printed) = (render.clone(), printed.clone());
        jinja.add_filter(
            decorator,
            move |input: MjValue, args: Rest<MjValue>, kwargs: Kwargs| {
                let keywords = kwargs.args().next().is_some();
                record(&render, level, decorator, &args, keywords, &printed)
                    .map_err(|message| render.fail(level, message))?;
                Ok::<_, Error>(input)
            },
        );
    }
    let (next, seen) = (render.clone(), printed.clone());
    jinja.add_filter("super", move |input: MjValue| {
        if input.downcast_object_ref::<Sentinel>().is_none() || seen.load(Ordering::SeqCst) > 0 {
            return Err(next.fail(level, MUST_DECORATE));
        }
        if level + 1 >= next.lines.len() {
            return Err(next.fail(level, "`| super` has nothing above it"));
        }
        render_level(&next, level + 1)?;
        Ok(input)
    });
    match jinja.render_str(&render.lines[level], context(render, &printed)) {
        Ok(out) if out.trim() == SENTINEL => Ok(()),
        Ok(_) => Err(render.fail(level, MUST_DECORATE)),
        Err(error) => {
            // An error our own code raised is already recorded; this one is
            // the engine's.
            if !render.has_failed() {
                render.fail(level, explain(render, &render.lines[level], &error));
            }
            Err(error)
        }
    }
}

/// Whether `value` is, or holds, the sentinel.
fn holds_result(value: &MjValue) -> bool {
    value.downcast_object_ref::<Sentinel>().is_some() || format!("{value:?}").contains(SENTINEL)
}

fn record(
    render: &Render,
    level: usize,
    name: &str,
    args: &[MjValue],
    keywords: bool,
    printed: &AtomicUsize,
) -> Result<(), String> {
    if keywords {
        return Err(format!("{name}(): takes no keyword arguments"));
    }
    if printed.load(Ordering::SeqCst) > 0
        || args.iter().any(|a| a.is_undefined() || holds_result(a))
    {
        return Err(format!("{name}(): arguments must be constants"));
    }
    let arity = |ok: bool, want: &str| match ok {
        true => Ok(()),
        false => Err(format!("{name}(): takes {want}")),
    };
    let mut recorded = render.recorded.lock().unwrap_or_else(|e| e.into_inner());
    let line = &mut recorded[level];
    match name {
        "poll" => {
            arity(
                (1..=2).contains(&args.len()),
                "an interval and optionally a count",
            )?;
            let count = match args.get(1) {
                Some(count) => count_of(name, count)?,
                None => DEFAULT_POLL_COUNT,
            };
            line.poll = Some((interval_of(name, &args[0])?, count));
        }
        "ttl" => {
            arity(args.len() == 1, "an interval")?;
            line.ttl = Some(interval_of(name, &args[0])?);
        }
        "failback" => {
            arity(args.len() == 2, "an interval and a count")?;
            // Interval first, as for `poll`. A number there is the old
            // count-first order: refused, never read the other way round.
            if args[0].kind() != ValueKind::String {
                return Err(format!(
                    "{name}(): takes the interval first, as a string like '1s', then the \
                     count: {name}('1s', 3)"
                ));
            }
            line.failback = Some(FailbackConfig {
                reattempts: count_of(name, &args[1])?,
                interval_secs: interval_of(name, &args[0])?,
            });
        }
        "budget" | "timeout" => {
            arity(args.len() == 1, "a duration")?;
            let duration = duration_of(name, &args[0])?;
            if duration.is_zero() {
                return Err(format!("{name}(): must be at least 1ms"));
            }
            if name == "budget" {
                line.budget = Some(duration);
            } else {
                line.timeout = Some(duration);
            }
        }
        "nearest" => {
            arity(args.is_empty(), "no arguments")?;
            line.nearest = true;
        }
        "watch" => {
            arity(!args.is_empty(), "at least one path")?;
            line.watch = Some(watch_paths(args, &render.vars)?);
        }
        _ => unreachable!("{name} is not a decorator"),
    }
    Ok(())
}

/// A duration: an integer of seconds or a string through `parse_duration`.
/// May be zero or sub-second; the caller says what it can take.
fn duration_of(name: &str, value: &MjValue) -> Result<Duration, String> {
    match value.kind() {
        ValueKind::Number => value
            .as_i64()
            .map(|n| Duration::from_secs(n.max(0) as u64))
            .ok_or_else(|| match name {
                "budget" | "timeout" => {
                    format!("{name}(): a number is whole seconds; write '50ms' for less")
                }
                _ => format!("{name}(): interval must be a whole number of seconds"),
            }),
        ValueKind::String => {
            let text = value.as_str().unwrap_or_default();
            crate::config::duration_arg(text).map_err(|e| format!("{name}(): {e}"))
        }
        _ => Err(format!(
            "{name}(): a duration is a number of seconds or a string like '30s'"
        )),
    }
}

/// An interval: whole seconds, from 1 s to a year.
fn interval_of(name: &str, value: &MjValue) -> Result<u64, String> {
    let duration = duration_of(name, value)?;
    if duration.is_zero() {
        return Err(format!("{name}(): interval must be at least 1s"));
    }
    if duration.subsec_nanos() != 0 {
        return Err(format!("{name}(): interval must be whole seconds"));
    }
    match duration.as_secs() {
        secs if secs > MAX_INTERVAL_SECS => {
            Err(format!("{name}(): interval must be at most 1 year"))
        }
        secs => Ok(secs),
    }
}

fn count_of(name: &str, value: &MjValue) -> Result<u32, String> {
    let count = (value.kind() == ValueKind::Number)
        .then(|| value.as_i64())
        .flatten()
        .filter(|n| *n >= 1)
        .ok_or_else(|| format!("{name}(): count must be a whole number of at least 1"))?;
    match u32::try_from(count) {
        Ok(count) if count <= MAX_COUNT => Ok(count),
        _ => Err(format!("{name}(): count must be at most {MAX_COUNT}")),
    }
}

/// Split `watch` entries by first character: `/`, `~` and `$` start an
/// absolute path (expanded now, from `vars`); anything else is relative to the
/// Source's directory.
fn watch_paths(
    args: &[MjValue],
    vars: &HashMap<String, String>,
) -> Result<(Vec<String>, Vec<String>), String> {
    let (mut patterns, mut abs_paths) = (vec![], vec![]);
    for arg in args {
        let Some(path) = arg.as_str().filter(|p| !p.is_empty()) else {
            return Err("watch(): a path is a non-empty string".into());
        };
        if path.starts_with(['/', '~', '$']) {
            if let Some(rest) = path.strip_prefix('~')
                && !(rest.is_empty() || rest.starts_with('/'))
            {
                return Err("watch(): ~user paths are not supported".into());
            }
            let expanded = expand_abs_path(path, vars)
                .ok_or_else(|| format!("watch(): cannot expand {path}"))?;
            abs_paths.push(expanded.to_string_lossy().into_owned());
        } else {
            patterns.push(path.to_string());
        }
    }
    Ok((patterns, abs_paths))
}

/// The load error for a render that failed in the engine, not in our filters.
/// A decorator whose arguments cannot render by themselves (an attribute of
/// an undefined name, an unknown function, an operation the sentinel
/// refuses) is named; the engine's error cannot say which call it was. (A
/// bare undefined name renders fine in `[...]`: `record` catches that one.)
fn explain(render: &Render, line: &str, error: &Error) -> String {
    for (decorator, args) in calls(line) {
        let printed = Arc::new(AtomicUsize::new(0));
        if render
            .base
            .render_str(&format!("{{{{ [{args}] }}}}"), context(render, &printed))
            .is_err()
        {
            return format!("{decorator}(): arguments must be constants");
        }
    }
    match (error.kind(), error.detail()) {
        // These details already name the problem: "filter pol is unknown".
        (ErrorKind::UnknownFilter, Some(detail)) => detail.to_string(),
        (kind, Some(detail)) => format!("{kind}: {detail}"),
        (kind, None) => kind.to_string(),
    }
}

/// The schedule decorators `line` names, once each: a call (`poll(...)`) or the
/// bare `| nearest`. A text scan, like [`explain`]'s: it can only over-report a
/// decorator's name written inside a string, and a provider line has no use for
/// one.
pub fn named_decorators(line: &str) -> Vec<&'static str> {
    let mut found: Vec<&'static str> = calls(line).into_iter().map(|(name, _)| name).collect();
    let bare_nearest = line.match_indices("nearest").any(|(at, _)| {
        let before = line[..at].trim_end();
        let after = &line[at + "nearest".len()..];
        before.ends_with('|') && !after.starts_with(|c: char| c.is_alphanumeric() || c == '_')
    });
    if bare_nearest {
        found.push("nearest");
    }
    found.sort_unstable();
    found.dedup();
    found
}

/// Each decorator call in `line` with its argument text.
fn calls(line: &str) -> Vec<(&'static str, &str)> {
    let mut found = vec![];
    for decorator in DECORATORS {
        for (at, _) in line.match_indices(decorator) {
            if line[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_' || c == '.') {
                continue;
            }
            let Some(args) = line[at + decorator.len()..].trim_start().strip_prefix('(') else {
                continue;
            };
            let (mut depth, mut quote) = (1, None);
            for (i, c) in args.char_indices() {
                match (quote, c) {
                    (Some(q), c) if c == q => quote = None,
                    (Some(_), _) => {}
                    (None, '\'' | '"') => quote = Some(c),
                    (None, '(') => depth += 1,
                    (None, ')') => {
                        depth -= 1;
                        if depth == 0 {
                            found.push((decorator, &args[..i]));
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::lifecycle::{
        DEFAULT_BUDGET, DEFAULT_FAILBACK_COUNT, DEFAULT_FAILBACK_INTERVAL_SECS, DEFAULT_POLL_COUNT,
        DEFAULT_TIMEOUT,
    };

    fn defaults() -> EngineDefaults {
        EngineDefaults {
            failback: FailbackConfig {
                reattempts: DEFAULT_FAILBACK_COUNT,
                interval_secs: DEFAULT_FAILBACK_INTERVAL_SECS,
            },
            budget: DEFAULT_BUDGET,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// What the expansion of an absolute `watch()` path reads: a map, never the process.
    fn test_vars() -> HashMap<String, String> {
        [("HOME", "/home/t"), ("XDG_CONFIG_HOME", "/xdg")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn record(lines: &[&str]) -> Result<Recorded, String> {
        record_return(lines, "status", &defaults(), &test_vars(), &return_env())
            .map_err(|e| e.message)
    }

    fn rec(line: &str) -> Recorded {
        record(&[line]).unwrap_or_else(|e| panic!("{line}: {e}"))
    }

    fn poll(interval_secs: u64, count: u32) -> InvalidationStrategy {
        InvalidationStrategy::Poll {
            interval_secs,
            count,
        }
    }

    fn watch(patterns: &[&str], abs: &[&str]) -> InvalidationStrategy {
        InvalidationStrategy::Watch {
            patterns: patterns.iter().map(|s| s.to_string()).collect(),
            abs_paths: abs.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn poll_only_is_poll_with_default_count() {
        let r = rec("{{ result | poll('60s') }}");
        assert_eq!(r.invalidation, poll(60, DEFAULT_POLL_COUNT));
        assert_eq!(r.decay, None);
        assert_eq!(rec("{{ result | poll(30, 4) }}").invalidation, poll(30, 4));
        assert_eq!(rec("{{ result | poll('2m') }}").invalidation, poll(120, 12));
    }

    #[test]
    fn watch_relative_and_absolute_split() {
        let r = rec("{{ result | watch('.git/index', '/etc/hosts', 'Cargo.toml') }}");
        assert_eq!(
            r.invalidation,
            watch(&[".git/index", "Cargo.toml"], &["/etc/hosts"])
        );
    }

    #[test]
    fn absolute_watch_entries_expand_from_the_given_variables() {
        let r = rec("{{ result | watch('~/a', '$HOME/b', '$XDG_CONFIG_HOME/c') }}");
        let InvalidationStrategy::Watch { abs_paths, .. } = r.invalidation else {
            panic!("watch only");
        };
        assert_eq!(abs_paths, ["/home/t/a", "/home/t/b", "/xdg/c"]);
    }

    #[test]
    fn absolute_watch_entries_do_not_read_the_process_environment() {
        // The process has a HOME; these variables do not, so there is nothing to expand from.
        assert!(std::env::var_os("HOME").is_some());
        let error = record_return(
            &["{{ result | watch('$XDG_CONFIG_HOME/mise') }}"],
            "s",
            &defaults(),
            &HashMap::new(),
            &return_env(),
        )
        .unwrap_err();
        assert_eq!(
            error.message,
            "watch(): cannot expand $XDG_CONFIG_HOME/mise"
        );
    }

    #[test]
    fn watch_and_poll_is_watch_and_poll() {
        let r = rec("{{ result | watch('.git/index') | poll('60s', 12) }}");
        assert_eq!(
            r.invalidation,
            InvalidationStrategy::WatchAndPoll {
                patterns: vec![".git/index".into()],
                abs_paths: vec![],
                interval_secs: 60,
                count: 12,
            }
        );
    }

    #[test]
    fn ttl_is_poll_n_1_decay_1() {
        let r = rec("{{ result | ttl('30s') }}");
        assert_eq!(r.invalidation, poll(30, 1));
        assert_eq!(r.decay, Some(1));
        let r = rec("{{ result | ttl(5) | watch('a') }}");
        assert_eq!(
            r.invalidation,
            InvalidationStrategy::WatchAndPoll {
                patterns: vec!["a".into()],
                abs_paths: vec![],
                interval_secs: 5,
                count: 1,
            }
        );
        assert_eq!(r.decay, Some(1));
    }

    #[test]
    fn ttl_with_poll_is_an_error() {
        let e = record(&["{{ result | ttl('30s') | poll('60s') }}"]).unwrap_err();
        assert!(e.contains("ttl()") && e.contains("poll()"), "{e}");
    }

    #[test]
    fn no_schedule_decorator_is_watch_only() {
        let r = rec("{{ result | failback('5s', 2) }}");
        assert_eq!(r.invalidation, watch(&[], &[]));
        assert_eq!(r.decay, None);
    }

    #[test]
    fn failback_and_budget_default_when_absent() {
        let r = rec("{{ result | poll('60s') }}");
        assert_eq!(r.failback, defaults().failback);
        assert_eq!(r.failback.reattempts, 3);
        assert_eq!(r.failback.interval_secs, 1);
        assert_eq!(r.budget, Duration::from_millis(50));
        let r = record_return(
            &["{{ result | poll('60s') }}"],
            "s",
            &EngineDefaults {
                budget: Duration::from_millis(200),
                ..defaults()
            },
            &test_vars(),
            &return_env(),
        )
        .unwrap();
        assert_eq!(r.budget, Duration::from_millis(200));
    }

    #[test]
    fn failback_on_a_prototype_changes_the_value() {
        let r = record(&[
            "{{ result | poll('10s') | super }}",
            "{{ result | failback('2s', 5) }}",
        ])
        .unwrap();
        assert_eq!(
            r.failback,
            FailbackConfig {
                reattempts: 5,
                interval_secs: 2
            }
        );
    }

    /// `failback(interval, count)`: the interval first, as for `poll`.
    #[test]
    fn failback_takes_the_interval_then_the_count() {
        assert_eq!(
            rec("{{ result | failback('5s', 2) }}").failback,
            FailbackConfig {
                reattempts: 2,
                interval_secs: 5
            }
        );
        assert_eq!(
            rec("{{ result | failback('1m', 7) }}").failback,
            FailbackConfig {
                reattempts: 7,
                interval_secs: 60
            }
        );
    }

    /// The count-first order is refused, never read the other way round: a
    /// number where the interval goes is the old order, and the error says
    /// which comes first.
    #[test]
    fn the_count_first_failback_order_is_an_error_that_says_the_order() {
        for line in [
            "{{ result | failback(3, '1s') }}",
            "{{ result | failback(3, 1) }}",
            "{{ result | failback(1, 3) }}",
            "{{ result | poll('60s', 12) | failback(5, '2s') }}",
        ] {
            assert_eq!(
                record(&[line]).unwrap_err(),
                "failback(): takes the interval first, as a string like '1s', then the count: \
                 failback('1s', 3)",
                "{line}"
            );
        }
        assert_eq!(
            record(&["{{ result | failback('1s', '3') }}"]).unwrap_err(),
            "failback(): count must be a whole number of at least 1"
        );
        assert_eq!(
            record(&["{{ result | failback('1s') }}"]).unwrap_err(),
            "failback(): takes an interval and a count"
        );
    }

    #[test]
    fn return_must_decorate_result() {
        for line in [
            "{{ 'x' }}",
            "{{ result | upper }}",
            "plain text",
            "{{ 'x' | poll('1s') }}",
        ] {
            let e = record(&[line]).unwrap_err();
            assert_eq!(e, "a Source's return must decorate result", "{line}");
        }
    }

    /// A return line may define and call a macro, as any Source line may; it
    /// may not import one, nor hold `include`, `extends` or `block`, on its
    /// own line or a prototype's.
    #[test]
    fn a_return_line_refuses_an_import_and_include_extends_and_block() {
        record(&["{% macro p() %}1s{% endmacro %}{{ result | poll(p()) }}"]).unwrap();
        for (line, why) in [
            (
                "{% import 'p:macro.x' as x %}{{ result | poll('1s') }}",
                "macros are client templates: a Source's line cannot import",
            ),
            (
                "{% block b %}{{ result | poll('1s') }}{% endblock %}",
                "{% block %} is refused: text is reused with a macro",
            ),
        ] {
            assert_eq!(record(&[line]).unwrap_err(), why, "{line}");
            let above = record_return(
                &["{{ result | super }}", line],
                "s",
                &defaults(),
                &test_vars(),
                &return_env(),
            )
            .unwrap_err();
            assert_eq!((above.level, above.message.as_str()), (1, why), "{line}");
        }
    }

    #[test]
    fn super_extends_the_prototype() {
        let r = record(&[
            "{{ result | watch('.git/index') | super }}",
            "{{ result | poll('60s', 12) | failback('1s', 3) }}",
        ])
        .unwrap();
        assert_eq!(
            r.invalidation,
            InvalidationStrategy::WatchAndPoll {
                patterns: vec![".git/index".into()],
                abs_paths: vec![],
                interval_secs: 60,
                count: 12,
            }
        );
        assert_eq!(
            r.failback,
            FailbackConfig {
                reattempts: 3,
                interval_secs: 1
            }
        );
    }

    #[test]
    fn super_with_nothing_above_is_an_error() {
        let e = record(&["{{ result | poll('1s') | super }}"]).unwrap_err();
        assert!(e.contains("super"), "{e}");
    }

    #[test]
    fn super_reaches_through_two_prototypes() {
        let r = record(&[
            "{{ result | watch('a') | super }}",
            "{{ result | failback('3s', 4) | super }}",
            "{{ result | poll('9s', 2) | budget('70ms') }}",
        ])
        .unwrap();
        assert_eq!(r.failback.reattempts, 4);
        assert_eq!(r.budget, Duration::from_millis(70));
        assert!(matches!(
            r.invalidation,
            InvalidationStrategy::WatchAndPoll {
                interval_secs: 9,
                count: 2,
                ..
            }
        ));
    }

    #[test]
    fn underscore_name_is_the_applied_name() {
        let r = record_return(
            &["{{ result | watch(_.name ~ '.pid') }}"],
            "deploy",
            &defaults(),
            &test_vars(),
            &return_env(),
        )
        .unwrap();
        assert_eq!(r.invalidation, watch(&["deploy.pid"], &[]));
    }

    #[test]
    fn decorator_arguments_must_be_constants() {
        for (line, decorator) in [
            ("{{ result | poll(env.X) }}", "poll"),
            ("{{ result | watch(data.path) }}", "watch"),
            ("{{ result | failback('1s', n) }}", "failback"),
            ("{{ result | budget(result) }}", "budget"),
            ("{{ result | ttl(sources) }}", "ttl"),
            ("{{ result | poll(result) }}", "poll"),
            ("{{ result | watch('.' ~ data.path) }}", "watch"),
            ("{{ result | poll(exec('echo 60')) }}", "poll"),
            ("{{ result | watch(read('/etc/hosts')) }}", "watch"),
            ("{{ result | watch(lup('.git')) }}", "watch"),
            ("{{ result | watch('x' ~ result) }}", "watch"),
            ("{{ result | poll(result | upper) }}", "poll"),
            ("{{ result | watch([result]) }}", "watch"),
            ("{{ result | poll(result | length) }}", "poll"),
            ("{{ result | watch(result | truncate(5)) }}", "watch"),
            ("{{ result | watch(result | replace('@', '')) }}", "watch"),
            ("{{ result | watch(result | first) }}", "watch"),
        ] {
            let e = record(&[line]).unwrap_err();
            assert_eq!(
                e,
                format!("{decorator}(): arguments must be constants"),
                "{line}"
            );
        }
    }

    #[test]
    fn literal_expressions_and_underscore_name_are_constants() {
        assert_eq!(
            rec("{{ result | watch('a' ~ 'b') | poll(2 * 30) }}").invalidation,
            InvalidationStrategy::WatchAndPoll {
                patterns: vec!["ab".into()],
                abs_paths: vec![],
                interval_secs: 60,
                count: 12,
            }
        );
        let r = record_return(
            &["{{ result | watch('.' ~ _.name ~ '.lock') }}"],
            "build",
            &defaults(),
            &test_vars(),
            &return_env(),
        )
        .unwrap();
        assert_eq!(r.invalidation, watch(&[".build.lock"], &[]));
    }

    #[test]
    fn a_later_duplicate_decorator_replaces_an_earlier_one() {
        let poll_30 = ("{{ result | poll('10s') | poll('30s', 4) }}", poll(30, 4));
        assert_eq!(rec(poll_30.0).invalidation, poll_30.1);
        assert_eq!(
            rec("{{ result | watch('a') | watch('b') }}").invalidation,
            watch(&["b"], &[])
        );
        assert_eq!(
            rec("{{ result | failback('1s', 1) | failback('2s', 5) }}").failback,
            FailbackConfig {
                reattempts: 5,
                interval_secs: 2
            }
        );
        assert_eq!(
            rec("{{ result | budget('10ms') | budget('80ms') }}").budget,
            Duration::from_millis(80)
        );
    }

    #[test]
    fn a_sources_own_decorator_beats_the_prototypes_across_super() {
        let r = record(&[
            "{{ result | poll('15s') | super }}",
            "{{ result | poll('60s', 12) }}",
        ])
        .unwrap();
        assert_eq!(r.invalidation, poll(15, 12));
        let r = record(&[
            "{{ result | poll('15s', 3) | super }}",
            "{{ result | poll('60s', 12) }}",
        ])
        .unwrap();
        assert_eq!(
            r.invalidation,
            poll(15, 3),
            "the prototype's count is dropped too"
        );
        // Within one line a later duplicate still replaces an earlier one,
        // wherever super sits.
        let r = record(&[
            "{{ result | poll('15s') | super | poll('20s', 2) }}",
            "{{ result | watch('w') }}",
        ])
        .unwrap();
        assert!(matches!(
            r.invalidation,
            InvalidationStrategy::WatchAndPoll {
                interval_secs: 20,
                count: 2,
                ..
            }
        ));
    }

    #[test]
    fn nearest_is_recorded() {
        assert!(
            !rec("{{ result | watch('w') }}").nearest,
            "cascade is the default"
        );
        assert!(rec("{{ result | watch('.git/HEAD') | nearest }}").nearest);
        assert!(rec("{{ result | nearest() | watch('w') }}").nearest);
        assert!(
            rec("{{ result | nearest | nearest }}").nearest,
            "a later duplicate in one line is harmless"
        );
    }

    #[test]
    fn a_prototypes_nearest_reaches_a_source_through_super() {
        let r = record(&[
            "{{ result | watch('w') | super }}",
            "{{ result | nearest }}",
        ])
        .unwrap();
        assert!(r.nearest);
        let r = record(&[
            "{{ result | nearest | super }}",
            "{{ result | watch('w') }}",
        ])
        .unwrap();
        assert!(r.nearest, "the nearer line's setting applies");
    }

    #[test]
    fn a_source_without_super_does_not_inherit_nearest() {
        let r = record(&["{{ result | watch('w') }}", "{{ result | nearest }}"]).unwrap();
        assert!(!r.nearest);
    }

    #[test]
    fn nearest_takes_no_arguments() {
        for line in ["{{ result | nearest(1) }}", "{{ result | nearest('git') }}"] {
            assert_eq!(
                record(&[line]).unwrap_err(),
                "nearest(): takes no arguments",
                "{line}"
            );
        }
    }

    #[test]
    fn ttl_and_poll_are_one_slot_across_super() {
        let r = record(&[
            "{{ result | ttl('5s') | super }}",
            "{{ result | poll('60s', 12) }}",
        ])
        .unwrap();
        assert_eq!(r.invalidation, poll(5, 1));
        assert_eq!(r.decay, Some(1));
        let r = record(&[
            "{{ result | poll('15s') | super }}",
            "{{ result | ttl('5s') }}",
        ])
        .unwrap();
        assert_eq!(r.invalidation, poll(15, DEFAULT_POLL_COUNT));
        assert_eq!(r.decay, None);
        let e = record(&[
            "{{ result | ttl('5s') | poll('60s') | super }}",
            "{{ result }}",
        ])
        .unwrap_err();
        assert!(e.contains("ttl()") && e.contains("poll()"), "{e}");
    }

    #[test]
    fn budget_takes_milliseconds() {
        assert_eq!(
            rec("{{ result | budget('50ms') }}").budget,
            Duration::from_millis(50)
        );
        assert_eq!(
            rec("{{ result | budget('1500ms') }}").budget,
            Duration::from_millis(1500)
        );
        assert_eq!(
            rec("{{ result | budget(2) }}").budget,
            Duration::from_secs(2)
        );
    }

    #[test]
    fn a_sub_second_poll_interval_is_an_error() {
        for (line, decorator) in [
            ("{{ result | poll('500ms') }}", "poll"),
            ("{{ result | ttl('500ms') }}", "ttl"),
            ("{{ result | failback('500ms', 3) }}", "failback"),
        ] {
            assert_eq!(
                record(&[line]).unwrap_err(),
                format!("{decorator}(): interval must be whole seconds"),
                "{line}"
            );
        }
    }

    #[test]
    fn bad_arguments_name_the_decorator() {
        for line in [
            "{{ result | poll() }}",
            "{{ result | poll('soon') }}",
            "{{ result | poll(0) }}",
            "{{ result | poll('10s', 0) }}",
            "{{ result | watch() }}",
            "{{ result | failback('1s') }}",
            "{{ result | budget('0ms') }}",
            "{{ result | poll('10s', foo=1) }}",
        ] {
            let e = record(&[line]).unwrap_err();
            let decorator = line.split("| ").nth(1).unwrap().split('(').next().unwrap();
            assert!(e.starts_with(&format!("{decorator}():")), "{line}: {e}");
        }
    }

    /// `timeout(d)` is how long one run of the Source may take before it is
    /// cut off; a Source whose chain does not say takes the engine's.
    #[test]
    fn timeout_is_recorded_and_otherwise_the_engines() {
        assert_eq!(
            rec("{{ result | timeout('2s') }}").timeout,
            Duration::from_secs(2)
        );
        assert_eq!(
            rec("{{ result | timeout('750ms') }}").timeout,
            Duration::from_millis(750)
        );
        assert_eq!(
            rec("{{ result | timeout(3) }}").timeout,
            Duration::from_secs(3)
        );
        assert_eq!(rec("{{ result | poll('60s') }}").timeout, DEFAULT_TIMEOUT);
        let r = record_return(
            &["{{ result | poll('60s') }}"],
            "s",
            &EngineDefaults {
                timeout: Duration::from_secs(4),
                ..defaults()
            },
            &test_vars(),
            &return_env(),
        )
        .unwrap();
        assert_eq!(r.timeout, Duration::from_secs(4));
        for line in ["{{ result | timeout('0s') }}", "{{ result | timeout(0) }}"] {
            assert_eq!(
                record(&[line]).unwrap_err(),
                "timeout(): must be at least 1ms",
                "{line}"
            );
        }
    }

    #[test]
    fn a_zero_budget_is_refused() {
        for line in [
            "{{ result | budget('0s') }}",
            "{{ result | budget('0') }}",
            "{{ result | budget(0) }}",
        ] {
            assert_eq!(
                record(&[line]).unwrap_err(),
                "budget(): must be at least 1ms",
                "{line}"
            );
        }
    }

    #[test]
    fn booleans_are_not_numbers() {
        assert_eq!(
            record(&["{{ result | poll(true) }}"]).unwrap_err(),
            "poll(): a duration is a number of seconds or a string like '30s'"
        );
        assert_eq!(
            record(&["{{ result | poll('60s', true) }}"]).unwrap_err(),
            "poll(): count must be a whole number of at least 1"
        );
    }

    #[test]
    fn keyword_arguments_are_refused_by_name() {
        for line in [
            "{{ result | poll('10s', foo=1) }}",
            "{{ result | poll(interval='60s') }}",
        ] {
            assert_eq!(
                record(&[line]).unwrap_err(),
                "poll(): takes no keyword arguments",
                "{line}"
            );
        }
    }

    #[test]
    fn a_decorators_own_error_is_reported_unchanged() {
        assert_eq!(
            record(&["{{ result | poll('soon') }}"]).unwrap_err(),
            format!(
                "poll(): invalid duration `soon`: {}",
                crate::config::DURATION_GRAMMAR
            )
        );
        assert_eq!(
            record(&["{{ result | pol('1s') }}"]).unwrap_err(),
            "filter pol is unknown"
        );
    }

    #[test]
    fn interval_messages_say_what_is_wrong() {
        for line in ["{{ result | poll(0) }}", "{{ result | poll(-5) }}"] {
            assert_eq!(
                record(&[line]).unwrap_err(),
                "poll(): interval must be at least 1s",
                "{line}"
            );
        }
        assert_eq!(
            record(&["{{ result | poll(1.5) }}"]).unwrap_err(),
            "poll(): interval must be a whole number of seconds"
        );
    }

    #[test]
    fn huge_intervals_and_counts_are_errors_not_panics() {
        assert_eq!(
            record(&["{{ result | poll('999999999999999999h') }}"]).unwrap_err(),
            format!(
                "poll(): invalid duration `999999999999999999h`: {}",
                crate::config::DURATION_GRAMMAR
            )
        );
        assert_eq!(
            record(&["{{ result | poll('9000000h') }}"]).unwrap_err(),
            "poll(): interval must be at most 1 year"
        );
        assert_eq!(
            record(&["{{ result | poll('1s', 10001) }}"]).unwrap_err(),
            "poll(): count must be at most 10000"
        );
        assert!(
            record(&["{{ result | poll('365d') }}"]).is_ok(),
            "a day is a unit"
        );
        assert_eq!(
            record(&["{{ result | poll('366d') }}"]).unwrap_err(),
            "poll(): interval must be at most 1 year"
        );
        assert!(record(&["{{ result | poll(31536000, 10000) }}"]).is_ok());
    }

    #[test]
    fn a_user_home_path_is_refused() {
        assert_eq!(
            record(&["{{ result | watch('~user/x') }}"]).unwrap_err(),
            "watch(): ~user paths are not supported"
        );
    }

    #[test]
    fn super_inside_a_string_is_not_super() {
        let r = rec("{{ result | watch('a|super') }}");
        assert_eq!(r.invalidation, watch(&["a|super"], &[]));
    }

    #[test]
    fn an_error_names_the_level_that_raised_it() {
        let at = |lines: &[&str]| {
            let e =
                record_return(lines, "s", &defaults(), &test_vars(), &return_env()).unwrap_err();
            (e.level, e.message)
        };
        assert_eq!(
            at(&["{{ result | watch() }}"]),
            (0, "watch(): takes at least one path".to_string())
        );
        assert_eq!(
            at(&[
                "{{ result | poll('1s') | super }}",
                "{{ result | poll('soon') }}"
            ]),
            (
                1,
                format!(
                    "poll(): invalid duration `soon`: {}",
                    crate::config::DURATION_GRAMMAR
                )
                .to_string()
            )
        );
        assert_eq!(
            at(&["{{ result | super }}", "{{ result | poll(env.X) }}"]),
            (1, "poll(): arguments must be constants".to_string())
        );
        assert_eq!(
            at(&["{{ result | super }}", "{{ result | pol('1s') }}"]),
            (1, "filter pol is unknown".to_string())
        );
        assert_eq!(
            at(&["{{ result | super }}"]),
            (0, "`| super` has nothing above it".to_string())
        );
        assert_eq!(
            at(&[
                "{{ result | super }}",
                "{{ result | ttl('5s') | poll('9s') }}"
            ]),
            (1, "ttl() and poll() cannot be combined".to_string())
        );
        let shown = record_return(
            &["{{ result | watch() }}"],
            "s",
            &defaults(),
            &test_vars(),
            &return_env(),
        )
        .unwrap_err()
        .to_string();
        assert_eq!(shown, "watch(): takes at least one path");
    }

    #[test]
    fn a_budget_number_is_whole_seconds() {
        assert_eq!(
            rec("{{ result | budget(2) }}").budget,
            Duration::from_secs(2)
        );
        assert_eq!(
            record(&["{{ result | budget(1.5) }}"]).unwrap_err(),
            "budget(): a number is whole seconds; write '50ms' for less"
        );
    }

    #[test]
    fn engine_errors_read_as_kind_and_detail() {
        for (line, message) in [
            ("{{ result | pol('1s') }}", "filter pol is unknown"),
            ("{{ nope('1s') }}", "unknown function: "),
            ("{{ result | poll('1s' }}", "syntax error: "),
            ("{{ missing.attr }}", "undefined value"),
        ] {
            let e = record(&[line]).unwrap_err();
            assert!(e.starts_with(message), "{line}: {e}");
            assert!(!e.contains("(in "), "{line}: {e}");
        }
    }

    /// A return line renders on the pure library: its filters and functions
    /// work, and the ones that do IO are not there.
    #[test]
    fn a_return_line_renders_on_the_pure_library() {
        let r =
            rec("{% set d = '/a/b/c.txt' | dirname | basename | upper %}{{ result | poll('2s') }}");
        assert_eq!(r.invalidation, poll(2, 12));
        let colour = "{% set c = 'x' | fg('#ff0000') | bold %}{{ result | poll('2s') }}";
        assert!(record(&[colour]).is_ok());
        let e = record(&["{% set d = 'x' | read %}{{ result | poll('2s') }}"]).unwrap_err();
        assert!(e.contains("filter read is unknown"), "{e}");
    }

    #[test]
    fn a_return_line_cannot_read_the_clock() {
        for call in [
            "now()",
            "1 | strftime('%F')",
            "'x' | fromiso",
            "1 | ago",
            "spinner('ab')",
        ] {
            let e = record(&[&format!(
                "{{% set t = {call} %}}{{{{ result | poll('2s') }}}}"
            )])
            .unwrap_err();
            assert!(e.contains("unknown"), "{call}: {e}");
        }
    }

    #[test]
    fn no_lines_is_an_error() {
        assert!(record(&[]).is_err());
    }
}
