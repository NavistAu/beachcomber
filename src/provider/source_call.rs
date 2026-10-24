//! A Source called from a line.
//!
//! A Source's `data` line reads its siblings (`sources.<s>()`) and any
//! provider's Source (`daemon.providers.<p>.sources.<s>()`); a provider's
//! `result` and `return` lines read the latter. [`source_call`] is the one
//! function behind all of them, and it answers the way a read of that Source
//! would, without the provider's lines, a `put` or a template: the Source's own
//! stored records, merged up its instance chain from the directory the call is
//! made for (a `| nearest` Source's chain is its one instance).
//!
//! The warm path only reads. The one cold branch runs the instance inline, once:
//! after it the record is stored, so a second call in the same read or refresh
//! finds it warm. It is only taken from a thread that may block (a Source's own
//! `data` line runs under `spawn_blocking`); a provider's lines render on the
//! read path and call with [`Cold::Skip`], the read having warmed what they
//! call first.
//!
//! A Source reached again while it is running, through however many others, is
//! a cycle: the error `sources: <p>.<s> calls itself`.

use crate::cache::{Cache, RecordFreshness};
use crate::provider::registry::{CalleeMap, ProviderRegistry};
use crate::provider::{SourceResult, Value};
use crate::scheduler::lifecycle::Key;
use libbeachcomber::eval::undeclared_names;
use minijinja::Environment;
use serde_json::Value as Json;
use std::cell::RefCell;
use std::sync::Arc;

/// What a call does with an instance that has nothing stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cold {
    /// Execute it inline. The caller may block.
    Run,
    /// Leave it: the call answers with what is stored. For a line that renders
    /// on the read path, where the read has already warmed what is called.
    Skip,
}

thread_local! {
    /// Why the last call on this thread failed, for the run that made it to
    /// report: see [`ran`].
    static FAILED_CALL: RefCell<Option<String>> = const { RefCell::new(None) };

    /// The instances running on this thread, outermost first: the innermost is
    /// the caller of any call made now.
    static RUNNING: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
}

/// One running instance, and what its run has called so far.
struct Frame {
    provider: String,
    source: String,
    called: CalleeMap,
}

/// A Source's run, for as long as it is held: a call to it from anything it
/// calls is a cycle.
///
/// The cycle check is a backstop. It sees only the Sources running on this
/// thread, so it catches a ring only when one run calls the next inline; it
/// relies on the load-time check of the calls between Sources being complete
/// (every call is named in a line), and a ring that check missed could, with
/// the runs on different threads, wait on itself.
pub struct Running(());

impl Running {
    pub fn enter(provider: &str, source: &str) -> Self {
        RUNNING.with(|r| {
            r.borrow_mut().push(Frame {
                provider: provider.into(),
                source: source.into(),
                called: CalleeMap::new(),
            })
        });
        Running(())
    }

    /// The Sources this run has called so far, and the instances it reached
    /// in each; taken, so a second call finds none. A run replaces what its
    /// instance is known to call with exactly this when it ends.
    pub fn take_calls(&self) -> CalleeMap {
        RUNNING.with(|r| {
            r.borrow_mut()
                .last_mut()
                .map(|frame| std::mem::take(&mut frame.called))
                .unwrap_or_default()
        })
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        RUNNING.with(|r| {
            r.borrow_mut().pop();
        });
    }
}

fn is_running(provider: &str, source: &str) -> bool {
    RUNNING.with(|r| {
        r.borrow()
            .iter()
            .any(|f| f.provider == provider && f.source == source)
    })
}

/// Note, against the run in progress on this thread, that it called
/// `provider`'s `source` at the instances of `chain`.
fn note_call(provider: &str, source: &str, chain: &[Option<String>]) {
    RUNNING.with(|r| {
        if let Some(frame) = r.borrow_mut().last_mut() {
            frame
                .called
                .insert((provider.into(), source.into()), chain.to_vec());
        }
    });
}

/// The lifecycle and failure key of one instance of a Source.
pub fn instance_key(provider: &str, source_name: &str, root: Option<&str>) -> Key {
    Key {
        provider: provider.to_string(),
        path: root.map(str::to_string),
        source: source_name.to_string(),
        node: String::new(),
    }
}

/// Whether the instance has failed enough times in a row that its failback
/// holds it back until the backoff ends.
pub fn in_failback_backoff(
    registry: &ProviderRegistry,
    provider: &str,
    source_name: &str,
    root: Option<&str>,
) -> bool {
    registry
        .failures
        .lock()
        .unwrap()
        .get(&instance_key(provider, source_name, root))
        .is_some_and(|state| state.is_suppressed())
}

/// Record how a run of one instance went, and store its result: what the
/// scheduler's execute and the inline cold-miss path both do after the Source
/// has run. A failure counts toward the instance's failback and writes nothing.
pub fn record_outcome(
    registry: &ProviderRegistry,
    cache: &Cache,
    provider: &str,
    source_name: &str,
    root: Option<&str>,
    outcome: Result<SourceResult, String>,
) -> Result<(), String> {
    let Some(source) = registry.source(provider, source_name) else {
        return Err(format!("{provider}.{source_name}: no such source"));
    };
    let meta = source.metadata();
    // A reload that retired this Source since the registry was taken makes the
    // result stale: neither it nor its failure counts, and nothing is written.
    registry
        .unless_retired(provider, source_name, || {
            let mut failures = registry.failures.lock().unwrap();
            let state = failures
                .entry(instance_key(provider, source_name, root))
                .or_insert_with(|| {
                    crate::scheduler::FailureState::new(
                        meta.failback.reattempts,
                        std::time::Duration::from_secs(meta.failback.interval_secs),
                    )
                });
            match outcome {
                Ok(result) => {
                    state.record_success();
                    drop(failures);
                    cache.put_source(
                        provider,
                        root,
                        source_name,
                        result.fields,
                        crate::cache::Schedule::of(meta),
                    );
                    Ok(())
                }
                Err(e) => {
                    state.record_failure();
                    Err(e)
                }
            }
        })
        .unwrap_or(Ok(()))
}

/// A Source's record as a call finds it, and how old it is.
#[derive(Debug, Clone)]
pub struct CalledRecord {
    pub value: Json,
    /// The oldest stored record behind `value`: a line that computes from this
    /// is as old as that.
    pub fresh: RecordFreshness,
}

/// What a call gives: the record, none when nothing is stored, or why it could
/// not be read.
pub type CallOutcome = Result<Option<CalledRecord>, String>;

/// A call's own fault (a cycle, a Source that does not exist): the error a
/// read shows. Not a called Source's failure, which is the caller's miss, as a
/// direct read of it would be.
fn call_error(message: String) -> String {
    FAILED_CALL.with(|f| *f.borrow_mut() = Some(message.clone()));
    message
}

/// `provider`'s Source `source` as a read from the directory `entry` (`None`
/// is `~`) would find it: its stored records at each instance of its chain,
/// merged nearest-first. `None` when nothing is stored.
///
/// With [`Cold::Run`] an instance of the chain that has nothing stored is
/// executed here, inline, once (a second caller waits and finds it stored, or
/// the failure of a run that began after it arrived), unless it is in failback
/// backoff. The instances reached are noted against the run that is calling, so
/// a read that demands its Source demands them too. As in a direct read, an
/// instance whose run failed is a miss and the chain goes on to the rest; only
/// a fault of the lines is the call's error: a Source that is running on this
/// thread already is `sources: <p>.<s> calls itself`.
///
/// With [`Cold::Skip`] nothing runs and no `root` line is rendered: the chain
/// is walked over the roots already kept, and an instance not yet resolved is
/// a miss.
pub fn source_call(
    registry: &ProviderRegistry,
    cache: &Cache,
    provider: &str,
    source: &str,
    entry: Option<&str>,
    cold: Cold,
) -> CallOutcome {
    let Some(src) = registry.source(provider, source) else {
        return Err(call_error(format!(
            "sources: {provider}.{source}: no such source"
        )));
    };
    if cold == Cold::Run && is_running(provider, source) {
        return Err(call_error(format!(
            "sources: {provider}.{source} calls itself"
        )));
    }
    // A disabled provider produces nothing, and nothing of it runs: a miss.
    // The call is noted all the same, with no instance: what the caller stored
    // depends on the answer, and switching the provider on changes it.
    if registry.is_disabled(provider) {
        if cold == Cold::Run {
            note_call(provider, source, &[]);
        }
        return Ok(None);
    }
    let entry = crate::query::normalise_path(entry);
    let chain = match cold {
        Cold::Run => crate::query::instance_chain(registry, provider, source, entry.as_deref())?,
        Cold::Skip => match kept_chain(registry, provider, source, entry.as_deref())? {
            Some(chain) => chain,
            None => return Ok(None),
        },
    };
    if cold == Cold::Run {
        for root in &chain {
            let root = root.as_deref();
            if cache.has_provider_records_for(provider, root, source)
                || in_failback_backoff(registry, provider, source, root)
            {
                continue;
            }
            // Callers racing for a cold instance (a refresh runs several
            // Sources at once) take turns: the first runs it, the rest find
            // it stored, or share the failure of a run that finished while
            // they waited.
            let lock = registry.cold_lock(provider, source, root);
            let arrived = lock.finished.load(std::sync::atomic::Ordering::SeqCst);
            let outcome = {
                let mut last = lock.last.blocking_lock();
                if cache.has_provider_records_for(provider, root, source)
                    // A run that failed while this caller waited may have put
                    // the instance into backoff, whether or not it finished
                    // before this caller arrived.
                    || in_failback_backoff(registry, provider, source, root)
                {
                    Ok(())
                } else if lock.finished.load(std::sync::atomic::Ordering::SeqCst) > arrived
                    && let Some(Err(failed)) = &*last
                {
                    Err(failed.clone())
                } else {
                    let dir = crate::query::instance_dir(root);
                    // The run is inline, inside this thread's own run: keep
                    // what the caller's failed call said, and give it back.
                    let prior = FAILED_CALL.with(|f| f.borrow_mut().take());
                    let (outcome, fault) = ran(|| src.execute(dir.as_deref()));
                    FAILED_CALL.with(|f| *f.borrow_mut() = prior);
                    let recorded = record_outcome(registry, cache, provider, source, root, outcome)
                        .map_err(|e| (e, fault.is_some()));
                    *last = Some(recorded.clone());
                    lock.finished
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    recorded
                }
            };
            registry.release_cold(provider, source, root, lock);
            // As a direct read of the Source: an instance that failed is a
            // miss, and the chain goes on to the rest; only a fault of the
            // lines (a Source that does not exist, a cycle) is the error.
            if let Err((message, true)) = outcome {
                return Err(call_error(message));
            }
        }
        note_call(provider, source, &chain);
    }
    let mut merged = Json::Object(Default::default());
    let mut oldest: Option<RecordFreshness> = None;
    for root in chain.iter().rev() {
        let Some(stored) = cache.get_source(provider, root.as_deref(), source) else {
            continue;
        };
        if stored.fields.is_empty() {
            continue;
        }
        deep_merge(&mut merged, &to_json(&stored.fields));
        if oldest
            .as_ref()
            .is_none_or(|o| stored.last_refreshed < o.last_refreshed)
        {
            oldest = Some(RecordFreshness {
                source: Arc::from(source),
                entry: root.as_deref().map(Arc::from),
                last_refreshed: stored.last_refreshed,
                expected_interval_secs: stored.expected_interval_secs,
            });
        }
    }
    Ok(oldest.map(|fresh| CalledRecord {
        value: merged,
        fresh,
    }))
}

/// The instance chain of a Source from `dir`, walked without rendering a
/// `root` line: a Source that renders its root is asked only for roots already
/// kept. `None` when a root is not resolved yet.
fn kept_chain(
    registry: &ProviderRegistry,
    provider: &str,
    source: &str,
    dir: Option<&str>,
) -> Result<Option<Vec<Option<String>>>, String> {
    registry
        .instance_walk(provider, source, dir)
        .collect(|root_dir| match registry.renders_root(provider, source) {
            true => Ok(registry.kept_root(provider, source, root_dir)),
            false => registry.instance_root(provider, source, root_dir).map(Some),
        })
}

/// Run `execute` (a Source's run, on this thread) and say, beside its result,
/// why a call it made failed, if one did. A Source whose run fails because it
/// called a Source that does not exist, or itself, is a fault of the lines,
/// which a read shows; any other failure, a called Source's own included, stays
/// a miss.
pub fn ran<T>(execute: impl FnOnce() -> Result<T, String>) -> (Result<T, String>, Option<String>) {
    FAILED_CALL.with(|f| *f.borrow_mut() = None);
    let result = execute();
    let failed_call = FAILED_CALL.with(|f| f.borrow_mut().take());
    (result, failed_call)
}

fn to_json(fields: &std::collections::HashMap<String, Value>) -> Json {
    serde_json::to_value(fields).unwrap_or(Json::Null)
}

/// `over` laid on `base`: objects merge per key, anything else replaces.
fn deep_merge(base: &mut Json, over: &Json) {
    match (base, over) {
        (Json::Object(base), Json::Object(over)) => {
            for (k, v) in over {
                match base.get_mut(k) {
                    Some(existing) => deep_merge(existing, v),
                    None => {
                        base.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (base, over) => *base = over.clone(),
    }
}

/// A Source a line names to call.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Called {
    /// `sources.<s>()`: a sibling of the Source whose `data` line this is.
    Sibling(String),
    /// `daemon.providers.<p>.sources.<s>()`.
    Of(String, String),
}

/// The Sources `line` calls, each once, sorted. A `daemon` or `sources.` read
/// any other way than the call is an error: the call is the one way, and a
/// Source named any other way could not be warmed before the line renders.
/// A bare `sources` (the map itself, as `merge(sources)` takes it) is the
/// caller's to allow or refuse.
pub fn called_in(env: &Environment<'_>, line: &str) -> Result<Vec<Called>, String> {
    let names = undeclared_names(env, line, true)?;
    let mut out = std::collections::BTreeSet::new();
    for name in names {
        let parts: Vec<&str> = name.split('.').collect();
        match parts.as_slice() {
            ["sources"] => {}
            ["sources", source] => {
                out.insert(Called::Sibling((*source).to_string()));
            }
            ["daemon", "providers", provider, "sources", source] => {
                out.insert(Called::Of((*provider).to_string(), (*source).to_string()));
            }
            ["sources", ..] => {
                return Err(format!("{name}: a Source is called, sources.<source>()"));
            }
            ["daemon", ..] => {
                return Err(format!(
                    "{name}: a Source is called as daemon.providers.<provider>.sources.<source>()"
                ));
            }
            _ => {}
        }
    }
    Ok(out.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_running_on_the_thread_is_a_cycle() {
        assert!(!is_running("p", "s"));
        let guard = Running::enter("p", "s");
        assert!(is_running("p", "s"));
        assert!(!is_running("p", "t"));
        drop(guard);
        assert!(!is_running("p", "s"));
    }

    fn called(line: &str) -> Result<Vec<Called>, String> {
        called_in(&libbeachcomber::engine::build_pure_engine_env(), line)
    }

    #[test]
    fn a_line_names_the_sources_it_calls() {
        assert_eq!(
            called("{{ sources.b().x ~ daemon.providers.q.sources.s() ~ sources.b() }}").unwrap(),
            vec![
                Called::Sibling("b".into()),
                Called::Of("q".into(), "s".into())
            ]
        );
        assert_eq!(called("{{ merge(sources) }}").unwrap(), vec![]);
        assert!(called("{{ daemon.providers.q }}").is_err());
        assert!(called("{{ sources.b.x }}").is_err());
    }

    #[test]
    fn deeper_records_win_and_objects_merge() {
        let mut base = serde_json::json!({"a": 1, "o": {"x": 1, "y": 1}});
        deep_merge(&mut base, &serde_json::json!({"a": 2, "o": {"y": 2}}));
        assert_eq!(base, serde_json::json!({"a": 2, "o": {"x": 1, "y": 2}}));
    }
}
