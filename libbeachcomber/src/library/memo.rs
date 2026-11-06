//! Caching a template: `watch()`, `ttl()` and `cache()`, the filters that
//! memoise a client template leaf's value in the daemon.
//!
//! A leaf piped through `| cache()` is read once per change rather than once
//! per render: `cache()` hashes what the leaf depends on, and when the memo the
//! daemon holds for the leaf has that hash and is live, the render stops there
//! and the leaf's value is the memo's. Otherwise the leaf renders on, and the
//! resolver writes its final output back as the new memo, with the triggers
//! `watch()` and `ttl()` recorded before `cache()` ran.
//!
//! The filters keep no state of their own. What they know about the leaf
//! running now is the resolver's [`Frame`], on top of the render's
//! [`MemoFrames`] ([`EngineCtx::memo`]): a stack, because a lazy `comb` can
//! render a leaf while another waits on it. A render with no frames is the
//! daemon's, and the three are refused there; a [`Frame::Format`] (`-f`,
//! `comb eval`) passes its input through and records nothing.
//!
//! A hit ends the render at once, as an error carrying the memo ([`MemoHit`]),
//! which the evaluator turns back into the leaf's value
//! ([`crate::eval::evaluate`]). So whatever follows `cache()` in the leaf — an
//! `exec()` above all — never runs on a hit.

use super::{is_absent, is_absent_text};
use crate::engine::{EngineCtx, op_error};
use crate::protocol::Wait;
use crate::validity::{Hasher, hex};
use minijinja::value::{Kwargs, Rest, Value as MjValue, ValueKind};
use minijinja::{Environment, Error};
use serde_json::Value as JsonValue;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

pub(super) fn add_cache(env: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    let ctx = ctx.clone();
    env.add_filter(
        "cache",
        move |piped: MjValue, args: Rest<MjValue>, kwargs: Kwargs| {
            cache(&ctx, piped, &args, &kwargs)
        },
    );
}

pub(super) fn add_watch(env: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    let ctx = ctx.clone();
    env.add_filter("watch", move |piped: MjValue, args: Rest<MjValue>| {
        watch(&ctx, piped, &args)
    });
}

pub(super) fn add_ttl(env: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    let ctx = ctx.clone();
    env.add_filter("ttl", move |piped: MjValue, args: Rest<MjValue>| {
        ttl(&ctx, piped, &args)
    });
}

/// The memo the daemon holds for a leaf, as a `get`'s memo half carries it
/// (`{hash, stale, age_ms, data}`).
#[derive(Clone, Debug, PartialEq)]
pub struct Memo {
    /// The hash it was stored under, until an invalidation dropped it.
    pub hash: Option<String>,
    /// Past its `ttl`.
    pub stale: bool,
    pub age_ms: u128,
    /// The leaf's value; `None` for a memo of absence.
    pub data: Option<JsonValue>,
}

impl Memo {
    /// The memo one object of a memo half describes; `None` for anything
    /// that is not one.
    pub fn from_half(half: &JsonValue) -> Option<Memo> {
        let half = half.as_object()?;
        Some(Memo {
            hash: half.get("hash").and_then(JsonValue::as_str).map(Into::into),
            stale: half.get("stale").and_then(JsonValue::as_bool)?,
            age_ms: half.get("age_ms").and_then(JsonValue::as_u64)?.into(),
            data: half.get("data").cloned().filter(|d| !d.is_null()),
        })
    }
}

/// What `cache()` decided for a leaf.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cached {
    /// The memo answered: the render stopped at `cache()`.
    Hit,
    /// The leaf renders on, and its value is to be stored under this hash.
    Miss(u64),
}

/// A template leaf being rendered: what the memo filters read and record.
#[derive(Debug)]
pub struct Leaf {
    /// The leaf's address, `provider.field`: where its memo is stored.
    pub key: String,
    /// The memo the daemon holds for it, found up the chain from the
    /// resolution's directory.
    pub memo: Option<Memo>,
    /// `--force`: render past the memo.
    pub force: bool,
    /// How long the resolution waits ([`Wait`]).
    pub wait: Wait,
    /// What `watch()` recorded: absolute paths, in order, each once.
    pub paths: Vec<PathBuf>,
    /// What `ttl()` recorded.
    pub ttl: Option<Duration>,
    /// What `cache()` decided, once it has run.
    pub cached: Option<Cached>,
}

impl Leaf {
    pub fn new(key: String, memo: Option<Memo>, force: bool, wait: Wait) -> Self {
        Self {
            key,
            memo,
            force,
            wait,
            paths: Vec::new(),
            ttl: None,
            cached: None,
        }
    }

    /// Whether the memo answers for a leaf whose inputs hash to `hash`, never
    /// with `--force`: a live memo stored under the same hash; and for a
    /// reader that waits nothing (`--wait 0`), a stale one under the same
    /// hash or one whose hash its expiry dropped, rather than a render. A
    /// memo within its ttl whose hash was dropped was invalidated, and is a
    /// miss for every reader.
    fn takes(&self, hash: u64) -> bool {
        let Some(memo) = self.memo.as_ref().filter(|_| !self.force) else {
            return false;
        };
        let equal = memo.hash.as_deref() == Some(hex(hash).as_str());
        if !memo.stale {
            return equal;
        }
        self.wait == Wait::For(Duration::ZERO) && (equal || memo.hash.is_none())
    }
}

/// What the resolver knows about the render running now.
#[derive(Debug)]
pub enum Frame {
    /// A text rendered for a surface, stored nowhere: `-f`, `comb eval`, a
    /// `status -f` row. The filters pass their input through and record
    /// nothing; a field it reads caches as that field's template says.
    Format,
    /// A template leaf.
    Leaf(Leaf),
}

/// The frames of one resolution, innermost last: the resolver pushes one
/// around every render and pops it after, and the filters read the top.
#[derive(Debug, Default)]
pub struct MemoFrames(Mutex<Vec<Frame>>);

impl MemoFrames {
    fn frames(&self) -> MutexGuard<'_, Vec<Frame>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn push(&self, frame: Frame) {
        self.frames().push(frame);
    }

    pub fn pop(&self) -> Option<Frame> {
        self.frames().pop()
    }
}

/// How a hit ends the render: the error `cache()` returns, carrying the
/// memo's value (`None` for a memo of absence) as its source, which
/// [`crate::eval::evaluate`] finds and answers with.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoHit(pub Option<JsonValue>);

impl std::fmt::Display for MemoHit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("memo hit")
    }
}

impl std::error::Error for MemoHit {}

/// The hit an engine error carries, anywhere down its sources: an error
/// raised inside an imported macro comes back wrapped once per import.
pub fn hit_in(e: &Error) -> Option<&MemoHit> {
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(c) = cause {
        if let Some(hit) = c.downcast_ref::<MemoHit>() {
            return Some(hit);
        }
        cause = c.source();
    }
    None
}

/// The refusal of a memo filter where no client renders a leaf.
fn not_here(name: &str) -> Error {
    op_error(format!(
        "{name}(): a client template's filter; a Source's records are the daemon's"
    ))
}

/// Run `then` on the leaf being rendered; `Ok(None)` for a format, which
/// records nothing. No frame at all is the daemon's render.
fn on_leaf<T>(
    ctx: &EngineCtx,
    name: &str,
    then: impl FnOnce(&mut Leaf) -> Result<T, Error>,
) -> Result<Option<T>, Error> {
    let Some(frames) = &ctx.memo else {
        return Err(not_here(name));
    };
    let mut frames = frames.frames();
    match frames.last_mut() {
        None => Err(not_here(name)),
        Some(Frame::Format) => Ok(None),
        Some(Frame::Leaf(leaf)) => then(leaf).map(Some),
    }
}

/// `v | cache(inputs...)`: the read. The hash of the inputs — `v` when none
/// is given — and of every file `watch()` recorded, as each is now
/// ([`memo_hash`]). A live memo stored under it ends the render with its
/// value; otherwise `v` passes on, and the leaf's value is stored under the
/// hash once it has rendered.
fn cache(
    ctx: &EngineCtx,
    piped: MjValue,
    args: &[MjValue],
    kwargs: &Kwargs,
) -> Result<MjValue, Error> {
    kwargs
        .assert_all_used()
        .map_err(|_| op_error("cache(): takes its inputs, and no keyword"))?;
    let decided = on_leaf(ctx, "cache", |leaf| {
        if leaf.cached.is_some() {
            return Err(op_error("cache(): given twice"));
        }
        let inputs = if args.is_empty() {
            std::slice::from_ref(&piped)
        } else {
            args
        };
        let hash = memo_hash(inputs, &leaf.paths).map_err(op_error)?;
        if leaf.takes(hash) {
            leaf.cached = Some(Cached::Hit);
            let data = leaf.memo.as_ref().and_then(|m| m.data.clone());
            return Ok(Some(data));
        }
        leaf.cached = Some(Cached::Miss(hash));
        Ok(None)
    })?;
    match decided {
        Some(Some(data)) => Err(op_error("memo hit").with_source(MemoHit(data))),
        _ => Ok(piped),
    }
}

/// The hash `cache()` stores a leaf's memo under: FNV-1a 64
/// ([`crate::validity`]) over one part per input — `s` and the text for a
/// string, `j` and its JSON with keys sorted for anything else, `_` alone for
/// an absent one — then, for each path `watch()` recorded, its bytes and its
/// stamp as it is now ([`crate::fs::stamp`]). A file that changed since the
/// memo was stored gives another hash.
pub fn memo_hash(inputs: &[MjValue], paths: &[PathBuf]) -> Result<u64, String> {
    use std::os::unix::ffi::OsStrExt;
    let mut h = Hasher::default();
    for input in inputs {
        let part = if is_absent(input) {
            b"_".to_vec()
        } else if let Some(text) = input.as_str() {
            [b"s".as_slice(), text.as_bytes()].concat()
        } else {
            let json = serde_json::to_value(input)
                .and_then(|v| serde_json::to_vec(&v))
                .map_err(|e| format!("cache(): an input is not data: {e}"))?;
            [b"j".as_slice(), &json].concat()
        };
        h.part(&part);
    }
    for path in paths {
        h.part(path.as_os_str().as_bytes());
        h.part(&crate::fs::stamp(path).encode());
    }
    Ok(h.finish())
}

/// `v | watch(paths...)`: `v`, with each path recorded as a file whose change
/// invalidates the leaf's memo. A path is a string or a list of them; `none`
/// and `''` add nothing; with no argument the path is `v`. Each is read as
/// `read` reads one — `~` the render's `HOME`, a relative path against the
/// render's directory — then canonicalised when it is there, kept as written
/// when it is not.
fn watch(ctx: &EngineCtx, piped: MjValue, args: &[MjValue]) -> Result<MjValue, Error> {
    on_leaf(ctx, "watch", |leaf| {
        if leaf.cached.is_some() {
            return Err(op_error("watch(): comes before cache()"));
        }
        let mut named = Vec::new();
        if args.is_empty() {
            match piped.as_str() {
                Some(path) => named.push(path.to_string()),
                None if is_absent(&piped) => {}
                None => {
                    return Err(op_error(
                        "watch(): no path given, and the piped value is not one",
                    ));
                }
            }
        }
        for arg in args {
            paths_of(arg, &mut named)?;
        }
        for path in named.iter().filter(|p| !p.is_empty()) {
            let full = absolute(ctx, path)?;
            if !leaf.paths.contains(&full) {
                leaf.paths.push(full);
            }
        }
        Ok(())
    })?;
    Ok(piped)
}

/// The paths one `watch()` argument names, onto `out`: a string, or a list
/// of them however it was made (`env.KUBECONFIG | split(':')`).
fn paths_of(arg: &MjValue, out: &mut Vec<String>) -> Result<(), Error> {
    let not_a_path = || op_error("watch(): a path is a string, or a list of strings");
    if is_absent(arg) {
        return Ok(());
    }
    if let Some(path) = arg.as_str() {
        out.push(path.to_string());
        return Ok(());
    }
    // A list, or what `split` and the other lazy filters give, which the
    // engine iterates without collecting.
    if !matches!(arg.kind(), ValueKind::Seq | ValueKind::Iterable) {
        return Err(not_a_path());
    }
    for item in arg.try_iter()? {
        match item.as_str() {
            Some(path) => out.push(path.to_string()),
            None if is_absent(&item) => {}
            None => return Err(not_a_path()),
        }
    }
    Ok(())
}

/// `path` as the daemon watches it: read as the file items read a path
/// (`~` the render's `HOME`, relative against the render's directory), then
/// canonicalised when it is there.
fn absolute(ctx: &EngineCtx, path: &str) -> Result<PathBuf, Error> {
    let full = super::fs::resolve_path(ctx, path).map_err(|e| op_error(format!("watch(): {e}")))?;
    if !full.is_absolute() {
        return Err(op_error(format!(
            "watch(): {path} is relative, and the render has no directory"
        )));
    }
    Ok(std::fs::canonicalize(&full).unwrap_or(full))
}

/// `v | ttl(d)`: `v`, with `d` recorded as how long the leaf's memo is
/// valid: one duration of whole seconds, at least `1s`, as `put --ttl` takes.
/// `none` records nothing.
fn ttl(ctx: &EngineCtx, piped: MjValue, args: &[MjValue]) -> Result<MjValue, Error> {
    on_leaf(ctx, "ttl", |leaf| {
        if leaf.cached.is_some() {
            return Err(op_error("ttl(): comes before cache()"));
        }
        let [d] = args else {
            return Err(op_error("ttl(): takes one duration"));
        };
        if is_absent_text(d) {
            return Ok(());
        }
        let d = crate::duration::from_value(d).map_err(|e| op_error(format!("ttl(): {e}")))?;
        if d.is_zero() {
            return Err(op_error("ttl(): interval must be at least 1s"));
        }
        if d.subsec_nanos() != 0 {
            return Err(op_error("ttl(): interval must be whole seconds"));
        }
        if leaf.ttl.replace(d).is_some() {
            return Err(op_error("ttl(): given twice"));
        }
        Ok(())
    })?;
    Ok(piped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::build_engine_env;
    use crate::library::testing::ctx_with;
    use serde_json::json;

    /// A render of `expr` as the leaf `p.f`, with `memo` the daemon's: the
    /// expression's value, or its error's text, and the frame it left.
    fn leaf_render(
        ctx: Arc<EngineCtx>,
        memo: Option<Memo>,
        force: bool,
        expr: &str,
    ) -> (Result<String, String>, Leaf) {
        let frames = ctx.memo.clone().expect("a client ctx");
        frames.push(Frame::Leaf(Leaf::new(
            "p.f".into(),
            memo,
            force,
            Wait::Budget,
        )));
        let env = build_engine_env(ctx);
        let out = env
            .compile_expression(expr)
            .and_then(|e| e.eval(()))
            .map(|v| v.to_string());
        let out = match out {
            Ok(v) => Ok(v),
            Err(e) => match hit_in(&e) {
                Some(MemoHit(data)) => Ok(format!("hit {data:?}")),
                None => Err(e.to_string()),
            },
        };
        let Some(Frame::Leaf(leaf)) = frames.pop() else {
            panic!("the leaf's frame");
        };
        (out, leaf)
    }

    /// A client ctx: an environment, a directory, and a stack of frames.
    fn client(env: &[(&str, &str)], cwd: Option<&std::path::Path>) -> Arc<EngineCtx> {
        let mut ctx = ctx_with(env, cwd);
        Arc::get_mut(&mut ctx).unwrap().memo = Some(Arc::default());
        ctx
    }

    fn render(expr: &str) -> (Result<String, String>, Leaf) {
        leaf_render(client(&[], None), None, false, expr)
    }

    fn live(hash: u64, data: Option<JsonValue>) -> Memo {
        Memo {
            hash: Some(hex(hash)),
            stale: false,
            age_ms: 7,
            data,
        }
    }

    fn hash_of(inputs: &[MjValue]) -> u64 {
        memo_hash(inputs, &[]).unwrap()
    }

    #[test]
    fn watch_records_each_path_once_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap();
        std::fs::write(real.join("a"), "").unwrap();
        let ctx = client(&[("HOME", real.to_str().unwrap())], Some(&real));
        let (out, leaf) = leaf_render(
            ctx,
            None,
            false,
            &format!(
                "'v' | watch('a', ['~/b', none, ''], none, 'a') | watch('{}/a')",
                real.display()
            ),
        );
        assert_eq!(out.unwrap(), "v");
        assert_eq!(leaf.paths, [real.join("a"), real.join("b")]);
    }

    /// A colon list split by the template, as `KUBECONFIG` is, names each of
    /// its paths: `split` gives a lazy sequence, not a list.
    #[test]
    fn watch_takes_a_split_list() {
        let (out, leaf) = render("'v' | watch('/a:/b::/c' | split(':'))");
        assert_eq!(out.unwrap(), "v");
        assert_eq!(
            leaf.paths,
            [
                PathBuf::from("/a"),
                PathBuf::from("/b"),
                PathBuf::from("/c")
            ]
        );
    }

    /// A path that is there is canonicalised; one that is not is kept as it
    /// was read.
    #[test]
    fn watch_canonicalises_a_path_that_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap();
        std::fs::write(real.join("target"), "").unwrap();
        std::os::unix::fs::symlink(real.join("target"), real.join("link")).unwrap();
        let ctx = client(&[], Some(&real));
        let (_, leaf) = leaf_render(ctx, None, false, "1 | watch('link', 'nope/x', './target')");
        assert_eq!(
            leaf.paths,
            [real.join("target"), real.join("nope/x")],
            "the link and ./target are one file"
        );
    }

    #[test]
    fn watch_with_no_argument_watches_the_piped_path() {
        let ctx = client(&[], Some(std::path::Path::new("/r")));
        let (out, leaf) = leaf_render(ctx.clone(), None, false, "'x/y' | watch()");
        assert_eq!(out.unwrap(), "x/y");
        assert_eq!(leaf.paths, [PathBuf::from("/r/x/y")]);
        let (out, leaf) = leaf_render(ctx.clone(), None, false, "none | watch()");
        assert_eq!((out.unwrap(), leaf.paths), ("none".into(), vec![]));
        let (out, _) = leaf_render(ctx, None, false, "1 | watch()");
        assert!(
            out.unwrap_err()
                .contains("watch(): no path given, and the piped value is not one")
        );
    }

    #[test]
    fn watch_refuses_what_is_not_a_path() {
        for bad in ["'x' | watch(1)", "'x' | watch(['a', 2])", "'x' | watch({})"] {
            let (out, _) = render(bad);
            assert!(
                out.as_ref()
                    .unwrap_err()
                    .contains("watch(): a path is a string, or a list of strings"),
                "{bad}: {out:?}"
            );
        }
        let (out, _) = render("'x' | watch('rel')");
        assert!(
            out.as_ref()
                .unwrap_err()
                .contains("watch(): rel is relative, and the render has no directory"),
            "{out:?}"
        );
        let (out, _) = render("'x' | watch('~bob/x')");
        assert!(out.unwrap_err().contains("watch(): ~bob/x: only ~"));
    }

    #[test]
    fn ttl_records_whole_seconds_once() {
        let (out, leaf) = render("'v' | ttl('10m')");
        assert_eq!(out.unwrap(), "v");
        assert_eq!(leaf.ttl, Some(Duration::from_secs(600)));
        assert_eq!(render("'v' | ttl(90)").1.ttl, Some(Duration::from_secs(90)));
        assert_eq!(render("'v' | ttl(none)").1.ttl, None);
        for (bad, why) in [
            (
                "'v' | ttl('500ms')",
                "ttl(): interval must be whole seconds",
            ),
            ("'v' | ttl('0s')", "ttl(): interval must be at least 1s"),
            ("'v' | ttl('soon')", "ttl(): invalid duration `soon`"),
            ("'v' | ttl()", "ttl(): takes one duration"),
            ("'v' | ttl('1s', '2s')", "ttl(): takes one duration"),
            ("'v' | ttl('1s') | ttl('2s')", "ttl(): given twice"),
        ] {
            let (out, _) = render(bad);
            assert!(out.as_ref().unwrap_err().contains(why), "{bad}: {out:?}");
        }
    }

    /// The triggers are recorded before the read: after `cache()` they would
    /// not be in the hash, so they are an error there.
    #[test]
    fn the_triggers_come_before_cache() {
        let ctx = client(&[], Some(std::path::Path::new("/r")));
        for (bad, why) in [
            (
                "'v' | cache() | watch('a')",
                "watch(): comes before cache()",
            ),
            ("'v' | cache() | ttl('1m')", "ttl(): comes before cache()"),
            ("'v' | cache() | cache()", "cache(): given twice"),
        ] {
            let (out, _) = leaf_render(ctx.clone(), None, false, bad);
            assert!(out.as_ref().unwrap_err().contains(why), "{bad}: {out:?}");
        }
    }

    /// The daemon renders with no frames: the three are refused, by name.
    #[test]
    fn with_no_frame_the_filters_are_refused() {
        let env = build_engine_env(ctx_with(&[], None));
        for name in ["cache", "watch", "ttl"] {
            let err = env
                .compile_expression(&format!("'v' | {name}('1s')"))
                .unwrap()
                .eval(())
                .unwrap_err()
                .to_string();
            assert!(
                err.contains(&format!(
                    "{name}(): a client template's filter; a Source's records are the daemon's"
                )),
                "{err}"
            );
        }
    }

    /// A format records nothing and passes its input through: `-f` and
    /// `comb eval` render a cached pipeline as if it were not.
    #[test]
    fn a_format_frame_passes_through() {
        let ctx = client(&[], None);
        let frames = ctx.memo.clone().unwrap();
        frames.push(Frame::Format);
        let env = build_engine_env(ctx);
        let out = env
            .compile_expression("'v' | watch('/a') | ttl('500ms') | cache() | upper")
            .unwrap()
            .eval(())
            .unwrap();
        assert_eq!(out.as_str(), Some("V"));
        assert!(matches!(frames.pop(), Some(Frame::Format)));
    }

    /// A miss records the hash; a live memo under it is a hit carrying its
    /// value, one of absence included.
    #[test]
    fn cache_misses_then_hits_on_its_hash() {
        let (out, leaf) = render("'cmd' | cache() | upper");
        assert_eq!(out.unwrap(), "CMD");
        let hash = hash_of(&[MjValue::from("cmd")]);
        assert_eq!(leaf.cached, Some(Cached::Miss(hash)));

        let hit = |memo| {
            leaf_render(
                client(&[], None),
                Some(memo),
                false,
                "'cmd' | cache() | upper",
            )
        };
        let (out, leaf) = hit(live(hash, Some(json!("prod"))));
        assert_eq!(out.unwrap(), "hit Some(String(\"prod\"))");
        assert_eq!(leaf.cached, Some(Cached::Hit));
        assert_eq!(hit(live(hash, None)).0.unwrap(), "hit None");

        for memo in [
            live(hash ^ 1, Some(json!("prod"))),
            Memo {
                stale: true,
                ..live(hash, Some(json!("prod")))
            },
            Memo {
                hash: None,
                ..live(hash, Some(json!("prod")))
            },
        ] {
            let (out, leaf) = hit(memo.clone());
            assert_eq!(out.unwrap(), "CMD", "{memo:?}");
            assert_eq!(leaf.cached, Some(Cached::Miss(hash)));
        }
        let (out, _) = leaf_render(
            client(&[], None),
            Some(live(hash, Some(json!("prod")))),
            true,
            "'cmd' | cache() | upper",
        );
        assert_eq!(out.unwrap(), "CMD", "--force renders past the memo");
    }

    /// The hit table. A live memo answers under its own hash alone; a stale
    /// one answers only a reader that waits nothing, under its hash or with
    /// the hash its expiry dropped. A live memo whose hash was dropped was
    /// invalidated: a miss for every reader. `--force` takes nothing.
    #[test]
    fn the_hit_table() {
        let memo = |stale: bool, hash: Option<u64>| {
            Some(Memo {
                hash: hash.map(hex),
                stale,
                age_ms: 1,
                data: None,
            })
        };
        let takes =
            |memo: Option<Memo>, force, wait| Leaf::new("p.f".into(), memo, force, wait).takes(7);
        let zero = Wait::For(Duration::ZERO);
        let waits = Wait::For(Duration::from_millis(50));
        for (memo, others, waits_nothing) in [
            (memo(false, Some(7)), true, true),
            (memo(false, Some(8)), false, false),
            (memo(false, None), false, false),
            (memo(true, Some(7)), false, true),
            (memo(true, Some(8)), false, false),
            (memo(true, None), false, true),
            (None, false, false),
        ] {
            for wait in [Wait::Budget, waits, Wait::Forever] {
                assert_eq!(
                    takes(memo.clone(), false, wait),
                    others,
                    "{memo:?} {wait:?}"
                );
            }
            assert_eq!(takes(memo.clone(), false, zero), waits_nothing, "{memo:?}");
            assert!(!takes(memo, true, zero), "--force");
        }
    }

    /// Bare `cache()` hashes the piped value; arguments replace it. An absent
    /// argument is not `''`, and a map hashes as its data, however built.
    #[test]
    fn what_the_hash_covers() {
        let (_, bare) = render("'a' | cache()");
        let (_, argued) = render("'b' | cache('a')");
        assert_eq!(bare.cached, argued.cached, "the argument replaces the pipe");
        let (_, none) = render("'b' | cache(none)");
        let (_, empty) = render("'b' | cache('')");
        assert_ne!(none.cached, empty.cached, "absent is not ''");
        let (_, undefined) = render("'b' | cache(nope)");
        assert_eq!(
            none.cached, undefined.cached,
            "none and undefined are absent"
        );
        let (_, one) = render("'x' | cache({'a': 1, 'b': [2]})");
        let (_, two) = render("'x' | cache(dict(b=[2], a=1))");
        assert_eq!(one.cached, two.cached, "a map is its data");
        let (_, text) = render("'x' | cache('1')");
        let (_, number) = render("'x' | cache(1)");
        assert_ne!(text.cached, number.cached, "a string is not its JSON");
        let (_, pair) = render("'x' | cache('a', 'b')");
        let (_, joined) = render("'x' | cache('ab')");
        assert_ne!(pair.cached, joined.cached);
    }

    /// A watched file is in the hash: touching it changes the hash, and a
    /// file appearing does too.
    #[test]
    fn a_watched_file_is_in_the_hash() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap();
        let ctx = client(&[], Some(&real));
        let hashed = || {
            leaf_render(ctx.clone(), None, false, "'cmd' | watch('state') | cache()")
                .1
                .cached
        };
        let absent = hashed();
        std::fs::write(real.join("state"), "1").unwrap();
        let present = hashed();
        assert_ne!(absent, present, "its creation is a change");
        assert_eq!(present, hashed(), "nothing changed");
        std::fs::write(real.join("state"), "22").unwrap();
        assert_ne!(present, hashed(), "a rewrite is a change");
    }

    /// The golden vectors: hashes are stored, so these never change. Each is
    /// the validity hash over the parts the docs list.
    #[test]
    fn the_golden_vectors() {
        let bare = hash_of(&[MjValue::from("kubectl x")]);
        assert_eq!(bare, crate::validity::hash([b"skubectl x".as_slice()]));
        assert_eq!(hex(bare), "21730e37bbe8aa76");
        let absent = std::path::Path::new("/nonexistent-comb/kubeconfig");
        let full = memo_hash(
            &[
                MjValue::from("kubectl x"),
                MjValue::from(()),
                MjValue::from_serialize(json!({"b": 1, "a": [true]})),
            ],
            &[absent.to_path_buf()],
        )
        .unwrap();
        let parts: [&[u8]; 5] = [
            b"skubectl x",
            b"_",
            br#"j{"a":[true],"b":1}"#,
            b"/nonexistent-comb/kubeconfig",
            &[0],
        ];
        assert_eq!(full, crate::validity::hash(parts));
        assert_eq!(hex(full), "b4c32a85c10d28fc");
    }

    #[test]
    fn a_memo_half_reads_as_a_memo() {
        assert_eq!(
            Memo::from_half(
                &json!({"hash": "9c6001d328a94690", "stale": false, "age_ms": 4, "data": "prod"})
            ),
            Some(Memo {
                hash: Some("9c6001d328a94690".into()),
                stale: false,
                age_ms: 4,
                data: Some(json!("prod")),
            })
        );
        assert_eq!(
            Memo::from_half(&json!({"stale": true, "age_ms": 4})),
            Some(Memo {
                hash: None,
                stale: true,
                age_ms: 4,
                data: None
            })
        );
        assert_eq!(Memo::from_half(&json!("x")), None);
    }
}
