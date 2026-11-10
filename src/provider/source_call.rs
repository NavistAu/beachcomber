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
//! [`source_call`] only reads: a provider's lines render on the read path, which
//! has warmed what they call first. [`run_call`], a Source's `data` line's call,
//! runs a cold instance first, through the one run path ([`crate::run`]), and
//! waits for it: from a thread that may block (the calling Source's own run, on
//! the blocking pool). After it the record is stored, so a second call in the
//! same read or refresh finds it warm.
//!
//! A Source reached again while it is running, through however many others, is
//! a cycle: the error `sources: <p>.<s> calls itself`.

use crate::cache::{Cache, RecordFreshness, RecordState};
use crate::provider::Value;
use crate::provider::registry::{CalleeMap, ProviderRegistry};
use crate::query::Dir;
use crate::run::{Cycle, Outcome, Want};
use crate::scheduler::lifecycle::Key;
use libbeachcomber::eval::{Probe, undeclared_names};
use libbeachcomber::merge::deep_merge;
use serde_json::Value as Json;
use std::cell::RefCell;
use std::sync::Arc;

thread_local! {
    /// Why the last call on this thread failed, for the run that made it to
    /// report: see [`ran`].
    static FAILED_CALL: RefCell<Option<String>> = const { RefCell::new(None) };

    /// Whether the last run on this thread failed with `log.fatal`: see
    /// [`ran`].
    static FATAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };

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
/// The load-time check of the calls between Sources finds a ring before
/// anything runs (every call is named in a line). This is the backstop for the
/// Source running on this thread; a ring round runs on other threads is the
/// run path's to see ([`crate::run::Run::wait`]).
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
fn note_call(provider: &str, source: &str, chain: &[Dir]) {
    RUNNING.with(|r| {
        if let Some(frame) = r.borrow_mut().last_mut() {
            let roots = chain.iter().cloned().map(Dir::into_address).collect();
            frame.called.insert((provider.into(), source.into()), roots);
        }
    });
}

/// The lifecycle and failure key of one instance of a Source.
pub fn instance_key(provider: &str, source_name: &str, root: Option<&str>) -> Key {
    Key {
        provider: provider.to_string(),
        path: root.map(str::to_string),
        source: crate::cache::Origin::source(source_name),
        node: String::new(),
    }
}

/// The run on this thread has failed with `log.fatal`.
pub fn mark_fatal() {
    FATAL.set(true);
}

/// Whether the instance is held back: it has failed enough times in a row
/// that its failback holds it until the backoff ends, or a `log.fatal` has
/// stopped it, which no time ends. A stopped instance whose failed run's
/// inputs have changed since is re-armed here, one `stat` each, and runs.
pub fn in_failback_backoff(
    registry: &ProviderRegistry,
    provider: &str,
    source_name: &str,
    root: Option<&str>,
) -> bool {
    registry
        .failures
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_mut(&instance_key(provider, source_name, root))
        .is_some_and(|state| state.holds_back())
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
/// merged nearest-first. `None` when nothing is stored. Nothing runs and no
/// `root` line is rendered: the chain is walked over the roots already kept,
/// and an instance not yet resolved is a miss. For a provider's lines, which
/// render on the read path after the read has warmed what they call.
pub fn source_call(
    registry: &ProviderRegistry,
    cache: &Cache,
    provider: &str,
    source: &str,
    entry: Option<&str>,
) -> CallOutcome {
    if registry.source(provider, source).is_none() {
        return Err(call_error(format!(
            "sources: {provider}.{source}: no such source"
        )));
    }
    // A disabled provider or Source produces nothing: a miss.
    if registry.is_disabled(provider) || registry.is_source_disabled(provider, source) {
        return Ok(None);
    }
    let entry = Dir::named(entry, registry.home());
    Ok(kept_chain(registry, provider, source, &entry)?
        .and_then(|chain| stored(cache, provider, source, &chain)))
}

/// [`source_call`] for a Source's `data` line, from a thread that may block:
/// an instance of the chain that has nothing stored runs first, once, unless
/// it is in failback backoff (a second caller takes the same run, or finds it
/// stored), and the call waits for it. The instances reached are noted against
/// the run that is calling, so a read that demands its Source demands them
/// too. As in a direct read, an instance whose run failed is a miss and the
/// chain goes on to the rest; only a fault of the lines is the call's error: a
/// Source that is running on this thread already, or a run that would wait on
/// itself round a ring of runs, is `sources: <p>.<s> calls itself`.
pub fn run_call(
    registry: &Arc<ProviderRegistry>,
    cache: &Arc<Cache>,
    provider: &str,
    source: &str,
    entry: Option<&str>,
) -> CallOutcome {
    if registry.source(provider, source).is_none() {
        return Err(call_error(format!(
            "sources: {provider}.{source}: no such source"
        )));
    }
    let itself = || call_error(format!("sources: {provider}.{source} calls itself"));
    if is_running(provider, source) {
        return Err(itself());
    }
    let arrived = tokio::time::Instant::now();
    let chain = match warm_chain(registry, cache, provider, source, entry, arrived) {
        Ok(chain) => chain,
        Err(Unwarmed::Cycle) => return Err(itself()),
        Err(Unwarmed::Fault(message)) => return Err(call_error(message)),
    };
    Ok(stored(cache, provider, source, &chain))
}

/// Why [`warm_chain`] could not bring a chain up to date.
enum Unwarmed {
    /// Waiting for a run would close a ring of runs waiting on one another.
    Cycle,
    /// A fault of the lines: a `root` that could not be worked out, or a run
    /// that failed for one.
    Fault(String),
}

/// Run each instance of `provider`'s Source `source` reached from `entry` whose
/// record is not valid (nothing stored, past its lifetime, or an input
/// changed), wait for it to its end, and note the call against the run that is
/// calling. The chain reached. A Source's own read has no budget: a run
/// computes a value to store, and storing one built from a record it did not
/// wait for would defeat the record's validity. A disabled provider has no
/// instances: nothing of it runs, and the call is noted all the same, with
/// none, since what the caller stored depends on the answer and switching the
/// provider on changes it. An instance whose Source watches its tree is
/// checked once the file events from before `arrived` are heard, as a read of
/// it would be ([`tree_barrier`]).
fn warm_chain(
    registry: &Arc<ProviderRegistry>,
    cache: &Arc<Cache>,
    provider: &str,
    source: &str,
    entry: Option<&str>,
    arrived: tokio::time::Instant,
) -> Result<Vec<Dir>, Unwarmed> {
    if registry.is_disabled(provider) || registry.is_source_disabled(provider, source) {
        note_call(provider, source, &[]);
        return Ok(Vec::new());
    }
    let entry = Dir::named(entry, registry.home());
    let chain = crate::query::instance_chain(registry, provider, source, &entry)
        .map_err(Unwarmed::Fault)?;
    for root in &chain {
        let root = root.address();
        // A valid instance is the common call: answered before anything
        // shared is locked.
        if let Some(barrier) = tree_barrier(registry, cache, provider, source, root) {
            crate::run::block_on(barrier.pass(arrived));
        }
        let Some(want) = Want::for_read(record_state(registry, cache, provider, source, root))
        else {
            continue;
        };
        let key = instance_key(provider, source, root);
        let Some(run) = registry.runs().start(want, registry, cache, &key) else {
            continue;
        };
        match run.wait() {
            Err(Cycle) => return Err(Unwarmed::Cycle),
            // As a direct read of the Source: an instance whose run failed is
            // a miss, and the chain goes on to the rest; only a fault of the
            // lines (a Source that does not exist, a cycle) is the error.
            Ok(Outcome::Failed {
                message,
                fault: true,
            }) => return Err(Unwarmed::Fault(message)),
            Ok(_) => {}
        }
    }
    note_call(provider, source, &chain);
    Ok(chain)
}

/// What a read finds of the record of `provider`'s Source `source` at `root`,
/// counting what its last run called: a valid record whose callee has an
/// input changed, or was stored with a new value after it, is
/// [`RecordState::Invalid`], through any number of calls between Sources. A
/// callee past its lifetime, or with nothing stored, leaves it as it is: the
/// caller's own lifetime stands for those.
pub fn record_state(
    registry: &ProviderRegistry,
    cache: &Cache,
    provider: &str,
    source: &str,
    root: Option<&str>,
) -> RecordState {
    let own = cache.record_state(provider, root, source);
    if own == RecordState::Valid
        && callee_moved(registry, cache, provider, source, root, &mut Vec::new())
    {
        RecordState::Invalid
    } else {
        own
    }
}

/// Whether a callee of the instance at `root`, or of its callees, changed
/// since the instance's record was stored. `seen` stops a ring of calls.
// shortcut: a callee that changes while its caller runs, after the call, is
// older than the caller's record and goes unseen until the caller's next run;
// stamp each call with the callee's value if that window matters.
fn callee_moved(
    registry: &ProviderRegistry,
    cache: &Cache,
    provider: &str,
    source: &str,
    root: Option<&str>,
    seen: &mut Vec<Key>,
) -> bool {
    let Some(called) = registry.callees_of(provider, source, root) else {
        return false;
    };
    let key = instance_key(provider, source, root);
    if seen.contains(&key) {
        return false;
    }
    seen.push(key);
    let Some(stored) = cache
        .get_source(provider, root, source)
        .map(|r| r.last_refreshed)
    else {
        return false;
    };
    called.iter().any(|((p, s), roots)| {
        roots.iter().any(|r| {
            let r = r.as_deref();
            cache.record_state(p, r, s) == RecordState::Invalid
                || cache.changed_at(p, r, s).is_some_and(|at| at > stored)
                || callee_moved(registry, cache, p, s, r, seen)
        })
    })
}

/// The barrier a check of the instance at `root` waits on first, when its
/// Source watches its tree and that tree is watched: a change in the tree,
/// which no stat sees, is then in the record's state. `None` when there is
/// nothing to wait for.
pub fn tree_barrier<'c>(
    registry: &ProviderRegistry,
    cache: &'c Cache,
    provider: &str,
    source: &str,
    root: Option<&str>,
) -> Option<&'c Arc<crate::watcher::Barrier>> {
    let watches = registry
        .source(provider, source)
        .is_some_and(|s| crate::provider::watches_tree(&s.metadata().schedule));
    (watches && cache.tree_is_watched(provider, root, source))
        .then(|| cache.barrier())
        .flatten()
}

/// The stored records of `provider`'s Source `source` at the instances of
/// `chain` (nearest first), merged nearest-first; `None` when nothing is
/// stored.
fn stored(cache: &Cache, provider: &str, source: &str, chain: &[Dir]) -> Option<CalledRecord> {
    let mut merged = serde_json::Map::new();
    let mut oldest: Option<RecordFreshness> = None;
    for root in chain.iter().rev() {
        let Some(stored) = cache.get_source(provider, root.address(), source) else {
            continue;
        };
        if stored.fields.is_empty() {
            continue;
        }
        deep_merge(&mut merged, to_json(&stored.fields));
        if oldest
            .as_ref()
            .is_none_or(|o| stored.last_refreshed < o.last_refreshed)
        {
            oldest = Some(RecordFreshness {
                source: crate::cache::Origin::source(source),
                entry: root.address().map(Arc::from),
                last_refreshed: stored.last_refreshed,
                expected_interval_secs: stored.expected_interval_secs,
            });
        }
    }
    oldest.map(|fresh| CalledRecord {
        value: Json::Object(merged),
        fresh,
    })
}

/// Bring `provider` up to date for a read from `entry`, as a `get` of it
/// would before answering: each of its Sources, and each Source its lines
/// call, runs a cold instance of its chain once ([`warm_chain`]). What
/// `comb.<p>` in a `data` line does before it reads.
///
/// A run never waits on itself: a Source already running on this thread is
/// left as it stands, and so is one whose run is waiting, round a ring of
/// `comb.` reads between runs, on this one. A line that reads its own
/// provider, or reads round such a ring back to itself, finds that Source's
/// last stored value. Each instance reached is noted against the run that is
/// reading, so whatever keeps the reader warm keeps these warm too. Only a
/// fault of the lines is an error.
pub fn warm_provider(
    registry: &Arc<ProviderRegistry>,
    cache: &Arc<Cache>,
    provider: &str,
    entry: Option<&str>,
) -> Result<(), String> {
    let own = registry
        .provider_sources(provider)
        .unwrap_or_default()
        .iter()
        .filter(|s| !registry.is_source_disabled(provider, &s.name))
        .map(|s| (provider.to_string(), s.name.clone()));
    let called = registry.lines().all_calls(provider).iter().cloned();
    // One instant for every Source: a barrier passed for one serves the rest.
    let arrived = tokio::time::Instant::now();
    for (p, s) in own.chain(called).collect::<Vec<_>>() {
        if is_running(&p, &s) {
            continue;
        }
        match warm_chain(registry, cache, &p, &s, entry, arrived) {
            Ok(_) | Err(Unwarmed::Cycle) => {}
            Err(Unwarmed::Fault(message)) => return Err(call_error(message)),
        }
    }
    Ok(())
}

/// The instance chain of a Source from `dir`, walked without rendering a
/// `root` line: a Source that renders its root is asked only for roots already
/// kept. `None` when a root is not resolved yet.
fn kept_chain(
    registry: &ProviderRegistry,
    provider: &str,
    source: &str,
    dir: &Dir,
) -> Result<Option<Vec<Dir>>, String> {
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
/// a miss. Beside it, whether the run failed with `log.fatal` ([`mark_fatal`]).
pub fn ran<T>(
    execute: impl FnOnce() -> Result<T, String>,
) -> (Result<T, String>, Option<String>, bool) {
    FAILED_CALL.with(|f| *f.borrow_mut() = None);
    FATAL.set(false);
    let result = execute();
    let failed_call = FAILED_CALL.with(|f| f.borrow_mut().take());
    (result, failed_call, FATAL.take())
}

/// `fields` as JSON members. Serializing a `Value` cannot fail (a non-finite
/// float becomes `null`), so the `Null` fallback is never taken.
fn to_json(fields: &std::collections::HashMap<String, Value>) -> serde_json::Map<String, Json> {
    fields
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::to_value(v).unwrap_or(Json::Null)))
        .collect()
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
/// A bare `sources` (the map itself, as `_.merge(sources)` takes it) is the
/// caller's to allow or refuse.
pub fn called_in(probe: &Probe<'_>, line: &str) -> Result<Vec<Called>, String> {
    calls_in(&undeclared_names(probe, line, true)?)
}

/// [`called_in`] for a line whose nested names ([`undeclared_names`]) are
/// `names`: what a caller that has read the line already asks, without
/// reading it again.
pub fn calls_in(names: &std::collections::BTreeSet<String>) -> Result<Vec<Called>, String> {
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
        called_in(&Probe::new(libbeachcomber::engine::pure_engine_env()), line)
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
        assert_eq!(called("{{ _.merge(sources) }}").unwrap(), vec![]);
        assert!(called("{{ daemon.providers.q }}").is_err());
        assert!(called("{{ sources.b.x }}").is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn a_merged_record_is_as_old_as_the_oldest_it_took_from() {
        let cache = Cache::new();
        let put = |path: Option<&str>, x: i64| {
            let fields = std::collections::HashMap::from([(
                "x".to_string(),
                crate::provider::Value::Int(x),
            )]);
            cache.put_source(
                "p",
                path,
                "s",
                fields,
                &crate::provider::Schedule::default(),
                None,
            );
        };
        put(Some("/near"), 1);
        tokio::time::advance(std::time::Duration::from_secs(5)).await;
        put(None, 2);
        let chain = [Dir::At("/near".to_string()), Dir::Global];
        let record = stored(&cache, "p", "s", &chain).expect("both are stored");
        assert_eq!(record.value, serde_json::json!({"x": 1}));
        assert_eq!(record.fresh.entry.as_deref(), Some("/near"));
    }
}
