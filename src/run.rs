//! One run path for an instance, whoever starts it.
//!
//! A read that finds an instance cold, a poll, a file event, a refresh,
//! `--force` and a Source's call to another all start a run here, and an
//! instance has at most one run at a time: [`Runs::start`] hands the caller
//! the run in flight, or the one queued behind it, or starts one. The run
//! executes on the blocking pool under its Source's `timeout` and records
//! itself in a task of its own, so a caller that stops waiting (its
//! connection gone, its budget spent) takes nothing with it. A caller waits on
//! the [`Run`] it was handed: from a task ([`Run::outcome`]) or from a
//! blocking thread ([`Run::wait`]).
//!
//! A run waits for another only from its blocking thread, when a line of its
//! Source reads one that is cold. Waiting would close a ring when the run
//! waited on is itself waiting, however far round, on the one that would
//! wait: [`Run::wait`] says so instead ([`Cycle`]), and the caller reads the
//! last stored value. A Source never waits on itself.
//!
//! A run's stat inputs are the files its Source declares, stamped immediately
//! before it executes, and the files its lines read, each stamped immediately
//! before it is read: the run keeps that account ([`reads_here`]) for the
//! lines of its Source to fill.

use crate::cache::{Cache, Stamped, Validity};
use crate::provider::registry::ProviderRegistry;
use crate::provider::source_call::{in_failback_backoff, ran};
use crate::provider::{Source, SourceResult};
use crate::scheduler::lifecycle::Key;
use libbeachcomber::fs::Touched;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use tracing::{debug, warn};

/// What a run's execution gave: the Source's result with the validity of its
/// stat inputs (what it declared, stamped immediately before it began, and
/// what its lines read, each stamped before it was read), or its error and
/// whether it was a fault in the lines (a Source it calls failed, or it
/// called itself).
type Ran = Result<(SourceResult, Option<Validity>), (String, bool)>;

/// What a caller needs of a run of an instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    /// A record: the run in flight, else a new one, unless the instance has
    /// stored one by now. A cold read, a Source's call, an instance's first
    /// demand.
    Cold,
    /// The run in flight, else a new one. A poll, a read of a record past its
    /// lifetime.
    Any,
    /// A run that sees the inputs as they are now: the run in flight if its
    /// inputs are as it stamped them (or it has yet to stamp them), else one
    /// that begins at once in its place, the run queued behind it if there is
    /// one: what the other produces could only be thrown away, and the read
    /// has no budget to spend waiting for that. A read that found an input of
    /// the record changed.
    Changed,
    /// A run that begins after now: the one queued behind the run in flight,
    /// if there is one. A file event, a refresh.
    Fresh,
    /// [`Self::Fresh`], past the failback backoff: `--force`.
    Forced,
}

/// How a run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Its result was stored.
    Stored,
    /// It failed, its timeout included: nothing was written, and the failure
    /// counts toward the instance's failback. `fault` when it failed because
    /// a line called a Source that does not exist, or called itself: a fault
    /// of the lines, which a read shows.
    Failed { message: String, fault: bool },
    /// Nothing it produced counts: a reload retired or removed its Source,
    /// the failback held a queued run back, or the daemon stopped first.
    Dropped,
}

/// Waiting would close a ring of runs that wait on one another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cycle;

/// Every run of every instance, by the instance's key: shared by the
/// registries of one daemon ([`ProviderRegistry::adopt`] carries it over), so
/// a run under way at a reload is the one a caller on the new registry finds.
#[derive(Default)]
pub struct Runs {
    table: Mutex<Table>,
}

#[derive(Default)]
struct Table {
    slots: HashMap<Key, Slot>,
    /// Each run whose blocking thread waits on another instance's run (by the
    /// run, [`RunId`]: a run a reload overtook may still be executing beside
    /// the one that replaced it), to that instance.
    waits: HashMap<RunId, Key>,
}

/// A run, by the number its [`State`] was made with: never reused, so a
/// thread a run left behind (cut off at its timeout) cannot be taken for a
/// later run's.
type RunId = u64;

fn id(state: &State) -> RunId {
    state.id
}

/// The run of an instance in flight, and what follows it.
struct Slot {
    running: Arc<State>,
    queued: Option<Queued>,
    overtaken: Option<Overtaken>,
}

/// The one run queued behind the run in flight, and what it runs under.
struct Queued {
    state: Arc<State>,
    forced: bool,
    registry: Arc<ProviderRegistry>,
    cache: Arc<Cache>,
}

/// What a reload did to the Source of a run in flight ([`Runs::supersede`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overtake {
    /// It is gone or its body changed: the run is dropped, and so is any run
    /// queued behind it that was asked for under an older registry.
    Gone,
    /// It was rescheduled: the instance runs again under the Source now in
    /// force, and those waiting on the overtaken run wait on that one.
    Again,
}

/// A reload overtook the run in flight: what it produced is not written.
enum Overtaken {
    /// [`Overtake::Gone`], by the reload that put the registry of `epoch` in
    /// force.
    Gone {
        epoch: u64,
    },
    Again,
}

/// How one run stands, for everyone waiting on it.
struct State {
    id: RunId,
    /// The validity of the run's declared inputs, set once it has stamped
    /// them, immediately before it executes. Shared with the run's blocking
    /// thread, which holds this and not the state: who holds the state are the
    /// run's callers.
    stamped: Arc<std::sync::OnceLock<Option<Validity>>>,
    /// What the run's lines have read so far, each stamped before it was
    /// read: shared with its blocking thread as `stamped` is.
    reads: Arc<Touched>,
    end: Mutex<Option<End>>,
    ended: Condvar,
    notify: tokio::sync::Notify,
}

impl Default for State {
    fn default() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            stamped: Arc::default(),
            reads: Arc::default(),
            end: Mutex::default(),
            ended: Condvar::default(),
            notify: tokio::sync::Notify::default(),
        }
    }
}

#[derive(Clone)]
enum End {
    Done(Outcome),
    /// Overtaken by a reload, or by a read that found it behind the files: the
    /// run that replaces it ends for them.
    Moved(Arc<State>),
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl State {
    /// Whether the run reads the inputs as they are now: it has yet to stamp
    /// them, or has none, or they are as it stamped them (or cannot be known
    /// otherwise); and the files its lines have read so far are as they were
    /// when read. What they have yet to read, they will read as it is.
    fn sees_now(&self) -> bool {
        let unchanged = |validity: &Option<Validity>| {
            validity
                .as_ref()
                .is_none_or(|v| v.check() != libbeachcomber::validity::Check::Invalid)
        };
        self.stamped.get().is_none_or(unchanged)
            && unchanged(&Validity::of(Stamped::of(self.reads.touches())))
    }

    /// The run is over. The first word stands: one replaced in flight has told
    /// its waiters where to go before its own task comes back.
    fn finish(&self, end: End) {
        lock(&self.end).get_or_insert(end);
        self.ended.notify_all();
        self.notify.notify_waiters();
    }

    async fn ended(&self) -> End {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(end) = lock(&self.end).clone() {
                return end;
            }
            notified.await;
        }
    }

    fn ended_blocking(&self) -> End {
        let mut end = lock(&self.end);
        loop {
            if let Some(end) = end.clone() {
                return end;
            }
            end = self.ended.wait(end).unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// The run executing on a thread: which it is, and its account of what its
/// lines read.
struct Current {
    id: RunId,
    reads: Arc<Touched>,
}

thread_local! {
    /// The run executing on this thread, if any: what a wait from it is, and
    /// where its lines note what they read.
    static HERE: RefCell<Option<Current>> = const { RefCell::new(None) };
}

/// A run executes on this thread for as long as this is held.
struct Here(Option<Current>);

impl Here {
    fn enter(id: RunId, reads: Arc<Touched>) -> Self {
        Here(HERE.with(|h| h.replace(Some(Current { id, reads }))))
    }
}

impl Drop for Here {
    fn drop(&mut self) {
        HERE.with(|h| *h.borrow_mut() = self.0.take());
    }
}

/// The account of what the run executing on this thread reads: where a
/// Source's lines note each file they read, so that a later read checks its
/// record against them. `None` off a run's thread (a Source executed directly,
/// as a test does), which then records nothing.
pub fn reads_here() -> Option<Arc<Touched>> {
    HERE.with(|h| {
        h.borrow()
            .as_ref()
            .map(|current| Arc::clone(&current.reads))
    })
}

/// A caller's hold on a run.
#[derive(Clone)]
pub struct Run {
    key: Key,
    state: Arc<State>,
    runs: Arc<Runs>,
}

impl std::fmt::Debug for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Run").field("key", &self.key).finish()
    }
}

impl Run {
    /// How the run ended, however long it takes: its Source's timeout bounds
    /// it.
    pub async fn outcome(&self) -> Outcome {
        let mut moved: Option<Arc<State>> = None;
        loop {
            let state = moved.as_ref().unwrap_or(&self.state);
            match state.ended().await {
                End::Done(outcome) => return outcome,
                End::Moved(next) => moved = Some(next),
            }
        }
    }

    /// [`Self::outcome`] if the run ends by `deadline`; `None` if it does not
    /// (it goes on, and records itself). A deadline already past (`--wait 0`,
    /// a budget spent) is answered at once with how the run has ended, if it
    /// has: tokio's timer turns in whole milliseconds and rounds a deadline up
    /// to its next turn, so a wait on it would hold the caller for a turn.
    pub async fn outcome_by(&self, deadline: tokio::time::Instant) -> Option<Outcome> {
        if deadline <= tokio::time::Instant::now() {
            return self.outcome_now();
        }
        tokio::time::timeout_at(deadline, self.outcome()).await.ok()
    }

    /// How the run has ended, if it has, without waiting.
    fn outcome_now(&self) -> Option<Outcome> {
        let mut state = Arc::clone(&self.state);
        loop {
            let end = lock(&state.end).clone()?;
            match end {
                End::Done(outcome) => return Some(outcome),
                End::Moved(next) => state = next,
            }
        }
    }

    /// [`Self::outcome`], from a blocking thread. From the thread of a run,
    /// [`Cycle`] when waiting would close a ring: the run waited on is
    /// waiting, however far round, on this one (or is this one).
    pub fn wait(&self) -> Result<Outcome, Cycle> {
        let me = HERE.with(|h| h.borrow().as_ref().map(|current| current.id));
        if let Some(me) = me {
            // Follow the waits from the run in flight for the instance waited
            // on (a run queued behind it waits on it first): reaching this
            // run's own is a ring.
            let mut table = lock(&self.runs.table);
            let mut at = self.key.clone();
            for _ in 0..=table.waits.len() {
                let Some(slot) = table.slots.get(&at) else {
                    break;
                };
                let running = id(&slot.running);
                if running == me {
                    return Err(Cycle);
                }
                match table.waits.get(&running) {
                    Some(next) => at = next.clone(),
                    None => break,
                }
            }
            table.waits.insert(me, self.key.clone());
        }
        let mut moved: Option<Arc<State>> = None;
        let outcome = loop {
            let state = moved.as_ref().unwrap_or(&self.state);
            match state.ended_blocking() {
                End::Done(outcome) => break outcome,
                End::Moved(next) => moved = Some(next),
            }
        };
        if let Some(me) = me {
            lock(&self.runs.table).waits.remove(&me);
        }
        Ok(outcome)
    }
}

impl Runs {
    /// The run of `key` that `want` needs, started if there is none in
    /// flight, under `registry` into `cache`. `None` when no run will happen:
    /// the Source is unknown, the instance is in failback backoff (only
    /// [`Want::Forced`] runs past it), or ([`Want::Cold`]) it has a record.
    ///
    /// Must be called within the daemon's runtime (from a task, or from a
    /// blocking thread of it).
    pub fn start(
        self: &Arc<Self>,
        want: Want,
        registry: &Arc<ProviderRegistry>,
        cache: &Arc<Cache>,
        key: &Key,
    ) -> Option<Run> {
        let mut table = lock(&self.table);
        if let Some(slot) = table.slots.get_mut(key) {
            let state = match want {
                Want::Cold | Want::Any if slot.overtaken.is_none() => Arc::clone(&slot.running),
                // Stamped under the lock: one stat per input, and only when a
                // read found the record changed with a run in flight.
                Want::Changed if slot.overtaken.is_none() && slot.running.sees_now() => {
                    Arc::clone(&slot.running)
                }
                Want::Changed
                    if slot.overtaken.is_none()
                        && !in_failback_backoff(
                            registry,
                            &key.provider,
                            &key.source,
                            key.path.as_deref(),
                        ) =>
                {
                    let found = registry.source(&key.provider, &key.source)?;
                    slot.queue(registry, cache, false);
                    let queued = slot.queued.take().expect("queued just now");
                    let runtime = tokio::runtime::Handle::try_current().expect(
                        "a run needs the daemon's runtime: start it from a task or a blocking thread",
                    );
                    let replaced = std::mem::replace(&mut slot.running, Arc::clone(&queued.state));
                    drop(table);
                    replaced.finish(End::Moved(Arc::clone(&queued.state)));
                    self.launch(
                        &runtime,
                        key,
                        Arc::clone(&queued.state),
                        found,
                        &queued.registry,
                        &queued.cache,
                    );
                    return Some(self.run(key, queued.state));
                }
                // A run a reload overtook gives a caller nothing: it waits for
                // the run queued behind it.
                _ => slot.queue(registry, cache, want == Want::Forced),
            };
            return Some(self.run(key, state));
        }
        let (provider, source, root) = (&key.provider, &key.source, key.path.as_deref());
        if want == Want::Cold && cache.has_provider_records_for(provider, root, source) {
            return None;
        }
        if want != Want::Forced && in_failback_backoff(registry, provider, source, root) {
            debug!("Source '{provider}.{source}' at {root:?} is in failback backoff: not run");
            return None;
        }
        let found = registry.source(provider, source)?;
        let runtime = tokio::runtime::Handle::try_current()
            .expect("a run needs the daemon's runtime: start it from a task or a blocking thread");
        let state = Arc::new(State::default());
        table.slots.insert(
            key.clone(),
            Slot {
                running: Arc::clone(&state),
                queued: None,
                overtaken: None,
            },
        );
        drop(table);
        self.launch(&runtime, key, Arc::clone(&state), found, registry, cache);
        Some(self.run(key, state))
    }

    /// Whether a run of `key` is in flight.
    pub fn is_running(&self, key: &Key) -> bool {
        lock(&self.table).slots.contains_key(key)
    }

    /// How many callers hold the run of `key` in flight: what a test waits for
    /// before it lets the run through. Beside the callers, the slot and the
    /// run's own task hold it.
    #[cfg(test)]
    pub(crate) fn callers(&self, key: &Key) -> Option<usize> {
        lock(&self.table)
            .slots
            .get(key)
            .map(|slot| Arc::strong_count(&slot.running).saturating_sub(2))
    }

    /// The instances with a run in flight.
    pub fn in_flight(&self) -> Vec<Key> {
        lock(&self.table).slots.keys().cloned().collect()
    }

    /// The reload that put `registry` (and `cache`) in force overtook the run
    /// of `key` in flight, if there is one: nothing it produced is written,
    /// and a caller from now on waits for a run queued behind it. What
    /// happens to the instance is `how`.
    pub fn supersede(
        &self,
        key: &Key,
        how: Overtake,
        registry: &Arc<ProviderRegistry>,
        cache: &Arc<Cache>,
    ) {
        let mut table = lock(&self.table);
        let Some(slot) = table.slots.get_mut(key) else {
            return;
        };
        debug!("run of {key:?} overtaken by a reload: {how:?}");
        slot.overtaken = Some(match how {
            Overtake::Again => {
                slot.queue(registry, cache, false);
                Overtaken::Again
            }
            Overtake::Gone => Overtaken::Gone {
                epoch: registry.epoch(),
            },
        });
    }

    fn run(self: &Arc<Self>, key: &Key, state: Arc<State>) -> Run {
        Run {
            key: key.clone(),
            state,
            runs: Arc::clone(self),
        }
    }

    /// Execute the run of `key` on the blocking pool under its Source's
    /// timeout, in a task of its own that records the result and starts what
    /// is queued behind it. If the task is dropped unfinished (the daemon
    /// stopping), the run ends [`Outcome::Dropped`] all the same, so nothing
    /// waits on it for ever.
    fn launch(
        self: &Arc<Self>,
        runtime: &tokio::runtime::Handle,
        key: &Key,
        state: Arc<State>,
        source: Arc<dyn Source>,
        registry: &Arc<ProviderRegistry>,
        cache: &Arc<Cache>,
    ) {
        let here = id(&state);
        let reads = Arc::clone(&state.reads);
        let settle = Settle {
            runs: Arc::clone(self),
            key: key.clone(),
            registry: Arc::clone(registry),
            cache: Arc::clone(cache),
            state,
            done: false,
        };
        runtime.spawn(async move {
            let timeout = source.metadata().timeout;
            let dir = crate::query::instance_dir(settle.key.path.as_deref());
            // The run reads through the registry it began under, which its
            // Source's `comb.<p>` reader holds only weakly: kept here, a
            // reload mid-run cannot leave it reading a registry that has gone.
            let keep = Arc::clone(&settle.registry);
            let root = settle.key.path.clone();
            let stamped = Arc::clone(&settle.state.stamped);
            let (tree_cache, tree_key) = (Arc::clone(&settle.cache), settle.key.clone());
            let ran = tokio::time::timeout(
                timeout,
                tokio::task::spawn_blocking(move || {
                    let _registry = keep;
                    let _here = Here::enter(here, Arc::clone(&reads));
                    // Stamped immediately before the run begins, and each
                    // file its lines read immediately before it is read: a
                    // change while it runs fails the next read's check, which
                    // makes one run more, never a stale answer.
                    let invalidation = &source.metadata().invalidation;
                    let declared = Stamped::take(crate::provider::declared_inputs(
                        invalidation,
                        root.as_deref(),
                    ))
                    .with_tree(
                        if crate::provider::watches_tree(invalidation) {
                            tree_cache.tree_stamp(
                                &tree_key.provider,
                                root.as_deref(),
                                &tree_key.source,
                            )
                        } else {
                            None
                        },
                    );
                    let _ = stamped.set(Validity::of(declared.clone()));
                    let (result, failed_call) = ran(|| source.execute(dir.as_deref()));
                    let result = result.map(|result| {
                        let read = reads.touches();
                        let validity = if read.is_empty() {
                            stamped.get().cloned().flatten()
                        } else {
                            Validity::of(declared.and_read(read))
                        };
                        (result, validity)
                    });
                    (result, failed_call)
                }),
            )
            .await;
            let result = match ran {
                Ok(Ok((Ok(result), _))) => Ok(result),
                Ok(Ok((Err(message), failed_call))) => Err((message, failed_call.is_some())),
                Ok(Err(e)) => Err((format!("panicked: {e}"), false)),
                Err(_) => Err((crate::scheduler::timed_out(timeout), false)),
            };
            settle.finish(result);
        });
    }

    /// The run of `key` is over, with `result` (`None`: its task was dropped
    /// first). Under the table's lock, the one a reload overtakes a run
    /// under, it is either overtaken and writes nothing or recorded like any
    /// other run. Then what was queued behind it starts, or the slot goes.
    fn settle(
        self: &Arc<Self>,
        key: &Key,
        registry: &Arc<ProviderRegistry>,
        cache: &Arc<Cache>,
        state: &Arc<State>,
        result: Option<Ran>,
    ) {
        let mut table = lock(&self.table);
        // The slot is this run's only while it is the one running there.
        let Some(slot) = table
            .slots
            .get_mut(key)
            .filter(|slot| Arc::ptr_eq(&slot.running, state))
        else {
            drop(table);
            state.finish(End::Done(Outcome::Dropped));
            return;
        };
        let mut ended = Vec::new();
        let stopping = result.is_none();
        let end = match (slot.overtaken.take(), result) {
            (Some(Overtaken::Gone { epoch }), _) => {
                // Only a run asked for under the Source now in force runs.
                if slot
                    .queued
                    .as_ref()
                    .is_some_and(|queued| queued.registry.epoch() < epoch)
                    && let Some(queued) = slot.queued.take()
                {
                    ended.push(queued.state);
                }
                End::Done(Outcome::Dropped)
            }
            (Some(Overtaken::Again), _) => End::Moved(Arc::clone(
                &slot
                    .queued
                    .as_ref()
                    .expect("an overtaken run that runs again has its run queued")
                    .state,
            )),
            (None, None) => End::Done(Outcome::Dropped),
            (None, Some(result)) => End::Done(record(registry, cache, key, result)),
        };
        // What runs next: nothing once the daemon is stopping (or off its
        // runtime), nothing the failback holds back unless it was forced,
        // nothing whose Source the registry it runs under does not have.
        let runtime = tokio::runtime::Handle::try_current().ok();
        let next = slot.queued.take().and_then(|queued| {
            let (provider, source, root) = (&key.provider, &key.source, key.path.as_deref());
            let runs = !stopping
                && runtime.is_some()
                && (queued.forced
                    || !in_failback_backoff(&queued.registry, provider, source, root));
            match queued.registry.source(provider, source).filter(|_| runs) {
                Some(found) => Some((queued, found)),
                None => {
                    ended.push(queued.state);
                    None
                }
            }
        });
        match &next {
            Some((queued, _)) => slot.running = Arc::clone(&queued.state),
            None => {
                table.slots.remove(key);
            }
        }
        drop(table);
        state.finish(end);
        for dropped in ended {
            dropped.finish(End::Done(Outcome::Dropped));
        }
        if let (Some((queued, found)), Some(runtime)) = (next, runtime) {
            self.launch(
                &runtime,
                key,
                queued.state,
                found,
                &queued.registry,
                &queued.cache,
            );
        }
    }
}

impl Slot {
    /// The run queued behind the one in flight, queued now if there is none,
    /// to run under the newest registry that asked for it.
    fn queue(
        &mut self,
        registry: &Arc<ProviderRegistry>,
        cache: &Arc<Cache>,
        forced: bool,
    ) -> Arc<State> {
        let queued = self.queued.get_or_insert_with(|| Queued {
            state: Arc::default(),
            forced: false,
            registry: Arc::clone(registry),
            cache: Arc::clone(cache),
        });
        queued.forced |= forced;
        if registry.epoch() >= queued.registry.epoch() {
            queued.registry = Arc::clone(registry);
            queued.cache = Arc::clone(cache);
        }
        Arc::clone(&queued.state)
    }
}

/// A run's task's hold on its slot: [`Runs::settle`] when it finishes, or
/// when it is dropped unfinished.
struct Settle {
    runs: Arc<Runs>,
    key: Key,
    registry: Arc<ProviderRegistry>,
    cache: Arc<Cache>,
    state: Arc<State>,
    done: bool,
}

impl Settle {
    /// Settle with `result`. Marked done only once settled: should settling
    /// panic, the drop settles again with nothing, and the waiters are let go.
    fn finish(mut self, result: Ran) {
        self.runs.settle(
            &self.key,
            &self.registry,
            &self.cache,
            &self.state,
            Some(result),
        );
        self.done = true;
    }
}

impl Drop for Settle {
    fn drop(&mut self) {
        if !self.done {
            self.runs
                .settle(&self.key, &self.registry, &self.cache, &self.state, None);
        }
    }
}

/// Record how a run of `key` went, under `registry`: a success is stored and
/// ends a streak of failures; a failure counts toward the instance's failback
/// and writes nothing. A run whose Source a reload has retired since it began
/// is [`Outcome::Dropped`]: neither its result nor its failure counts.
fn record(registry: &ProviderRegistry, cache: &Cache, key: &Key, result: Ran) -> Outcome {
    let (provider, source_name, root) = (&key.provider, &key.source, key.path.as_deref());
    let Some(source) = registry.source(provider, source_name) else {
        return Outcome::Dropped;
    };
    let meta = source.metadata();
    registry
        .unless_retired(provider, source_name, || match result {
            Ok((result, validity)) => {
                // A success ends a streak of failures; an instance that never
                // failed has no state to end (the common run).
                let mut failures = lock(&registry.failures);
                if !failures.is_empty()
                    && let Some(state) = failures.get_mut(key)
                {
                    state.record_success();
                }
                drop(failures);
                cache.put_source(
                    provider,
                    root,
                    source_name,
                    result.fields,
                    crate::cache::Schedule::of(meta).with_validity(validity),
                );
                debug!("Executed source '{provider}.{source_name}' path={root:?}");
                Outcome::Stored
            }
            Err((message, fault)) => {
                lock(&registry.failures)
                    .entry(key.clone())
                    .or_insert_with(|| {
                        crate::scheduler::FailureState::new(
                            meta.failback.reattempts,
                            std::time::Duration::from_secs(meta.failback.interval_secs),
                        )
                    })
                    // The row and the verdict already name the Source.
                    .record_failure(
                        message
                            .strip_prefix(&format!("{provider}.{source_name}: "))
                            .unwrap_or(&message),
                    );
                warn!("Source '{provider}.{source_name}' failed: {message}");
                Outcome::Failed { message, fault }
            }
        })
        .unwrap_or_else(|| {
            debug!("Source '{provider}.{source_name}' retired by a reload: its run is dropped");
            Outcome::Dropped
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{
        FailbackConfig, FieldSchema, InvalidationStrategy, SourceMetadata, Value,
    };
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    /// A latch a run waits on until the test opens it.
    #[derive(Default)]
    struct Gate {
        open: Mutex<bool>,
        opened: Condvar,
    }

    impl Gate {
        fn open(&self) {
            *self.open.lock().unwrap() = true;
            self.opened.notify_all();
        }

        fn close(&self) {
            *self.open.lock().unwrap() = false;
        }

        fn wait(&self) {
            let mut open = self.open.lock().unwrap();
            while !*open {
                open = self.opened.wait(open).unwrap();
            }
        }
    }

    /// A Source that counts its runs, waits on its gate, and answers the
    /// count, or fails while `fail` is set.
    struct Counted {
        meta: SourceMetadata,
        runs: Arc<AtomicU32>,
        gate: Arc<Gate>,
        fail: Arc<std::sync::atomic::AtomicBool>,
    }

    impl Source for Counted {
        fn metadata(&self) -> &SourceMetadata {
            &self.meta
        }

        fn execute(&self, _: Option<&str>) -> Result<SourceResult, String> {
            let run = self.runs.fetch_add(1, Ordering::SeqCst) + 1;
            self.gate.wait();
            if self.fail.load(Ordering::SeqCst) {
                return Err("down".into());
            }
            let mut result = SourceResult::new();
            result.insert("n", Value::Int(i64::from(run)));
            Ok(result)
        }
    }

    struct Fixture {
        registry: Arc<ProviderRegistry>,
        cache: Arc<Cache>,
        runs: Arc<AtomicU32>,
        gate: Arc<Gate>,
        fail: Arc<std::sync::atomic::AtomicBool>,
        key: Key,
    }

    fn meta(failback: u32, timeout: Duration) -> SourceMetadata {
        SourceMetadata {
            name: "s".into(),
            fields: vec![FieldSchema { name: "n".into() }],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 60,
                count: 2,
            },
            failback: FailbackConfig {
                reattempts: failback,
                interval_secs: 3600,
            },
            decay: None,
            budget: crate::scheduler::lifecycle::DEFAULT_BUDGET,
            timeout,
            nearest: false,
        }
    }

    /// The provider `p` of the one [`Counted`] Source `s`.
    struct P(
        SourceMetadata,
        Arc<AtomicU32>,
        Arc<Gate>,
        Arc<std::sync::atomic::AtomicBool>,
    );

    impl P {
        fn sources(&self) -> Vec<Box<dyn Source>> {
            vec![Box::new(Counted {
                meta: self.0.clone(),
                runs: self.1.clone(),
                gate: self.2.clone(),
                fail: self.3.clone(),
            })]
        }
    }

    fn fixture(failback: u32, timeout: Duration) -> Fixture {
        fixture_of(meta(failback, timeout))
    }

    fn fixture_of(meta: SourceMetadata) -> Fixture {
        let runs = Arc::new(AtomicU32::new(0));
        let gate = Arc::new(Gate::default());
        let fail = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut registry = ProviderRegistry::new();
        registry
            .register_sources(
                "p",
                P(meta, runs.clone(), gate.clone(), fail.clone()).sources(),
            )
            .unwrap();
        Fixture {
            registry: Arc::new(registry),
            cache: Arc::new(Cache::new()),
            runs,
            gate,
            fail,
            key: crate::provider::source_call::instance_key("p", "s", None),
        }
    }

    impl Fixture {
        /// The registry a reload puts in force after this one: the same
        /// Source, the same runs, a later epoch.
        fn reloaded(&self) -> Arc<ProviderRegistry> {
            let mut next = ProviderRegistry::new();
            next.register_sources(
                "p",
                P(
                    meta(3, Duration::from_secs(60)),
                    self.runs.clone(),
                    self.gate.clone(),
                    self.fail.clone(),
                )
                .sources(),
            )
            .unwrap();
            let next = next.adopt(&self.registry, &self.cache);
            self.registry
                .retire(&next, &std::collections::HashSet::new());
            next
        }

        fn start(&self, want: Want) -> Option<Run> {
            self.registry
                .runs()
                .start(want, &self.registry, &self.cache, &self.key)
        }

        fn stored(&self) -> Option<Value> {
            self.cache
                .get_source("p", None, "s")
                .and_then(|s| s.fields.get("n").cloned())
        }

        async fn until_runs(&self, n: u32) {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while self.runs.load(Ordering::SeqCst) < n {
                assert!(std::time::Instant::now() < deadline, "never {n} runs");
                tokio::task::yield_now().await;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_run_in_flight_is_joined_and_a_fresh_ask_queues_one() {
        let f = fixture(3, Duration::from_secs(60));
        let first = f.start(Want::Cold).expect("a cold instance runs");
        f.until_runs(1).await;
        let joined = f.start(Want::Any).unwrap();
        let joined_cold = f.start(Want::Cold).unwrap();
        let fresh = [
            f.start(Want::Fresh).unwrap(),
            f.start(Want::Forced).unwrap(),
        ];
        assert_eq!(f.registry.runs().in_flight(), std::slice::from_ref(&f.key));
        f.gate.open();
        for run in [&first, &joined, &joined_cold] {
            assert_eq!(run.outcome().await, Outcome::Stored);
        }
        for run in &fresh {
            assert_eq!(run.outcome().await, Outcome::Stored);
        }
        assert_eq!(f.runs.load(Ordering::SeqCst), 2, "one run, then one queued");
        assert_eq!(f.stored(), Some(Value::Int(2)));
        assert!(f.registry.runs().in_flight().is_empty());
        assert!(
            f.start(Want::Cold).is_none(),
            "a stored instance needs no run"
        );
    }

    /// A read that found its record's inputs changed takes the run in flight
    /// only while that run's own stamp of them is as they are: a run that
    /// stamped them before the change cannot have seen it, so one begins at
    /// once in its place, and those waiting on it wait on that one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_changed_record_takes_the_run_in_flight_only_if_it_sees_the_change() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input");
        std::fs::write(&input, "1").unwrap();
        // Long since written: the run's stamp is not racy.
        std::fs::File::options()
            .write(true)
            .open(&input)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(10))
            .unwrap();
        let f = fixture_of(SourceMetadata {
            invalidation: InvalidationStrategy::Watch {
                patterns: Vec::new(),
                abs_paths: vec![input.to_string_lossy().into_owned()],
            },
            ..meta(3, Duration::from_secs(60))
        });
        let first = f.start(Want::Cold).unwrap();
        // Executing: it stamped its input first.
        f.until_runs(1).await;
        let joined = f.start(Want::Changed).unwrap();
        assert_eq!(
            f.registry.runs().callers(&f.key),
            Some(2),
            "the input as the run stamped it: the run in flight"
        );
        std::fs::write(&input, "22").unwrap();
        let replacing = f.start(Want::Changed).unwrap();
        assert_eq!(
            f.registry.runs().callers(&f.key),
            Some(2),
            "the input changed since: a run in its place, this read and the one it replaced"
        );
        // It begins without the other having ended: the gate is still shut.
        f.until_runs(2).await;
        assert_eq!(f.registry.runs().in_flight(), vec![f.key.clone()]);
        f.gate.open();
        for run in [&first, &joined, &replacing] {
            assert_eq!(run.outcome().await, Outcome::Stored);
        }
        assert_eq!(f.runs.load(Ordering::SeqCst), 2);
        assert_eq!(f.stored(), Some(Value::Int(2)), "the second run's");
    }

    /// A `DaemonRead` for a Source that reads no other provider.
    struct NoReads;

    impl crate::provider::config_source::DaemonRead for NoReads {
        fn read(&self, _: &str, _: Option<&str>) -> Result<Option<HashMap<String, Value>>, String> {
            Ok(None)
        }
        fn ran_calling(
            &self,
            _: &str,
            _: &str,
            _: Option<&str>,
            _: crate::provider::registry::CalleeMap,
        ) {
        }
        fn call(
            &self,
            _: &str,
            _: &str,
            _: Option<&str>,
        ) -> Result<Option<serde_json::Value>, String> {
            Ok(None)
        }
    }

    /// A config Source `p.s` whose `data` reads `state` and then waits on
    /// `gate` in a plugin call, counting each call in `entered`; it polls, so
    /// it declares no files.
    fn gated_config_source(
        state: &std::path::Path,
        gate: Arc<Gate>,
        entered: Arc<AtomicU32>,
    ) -> Arc<ProviderRegistry> {
        use crate::boundaries::library::{MockLibraryLoader, MockLoadedLibrary};
        use crate::provider::config_source::{ConfigSource, ConfigSourceSpec, ResultSpec};
        let mut loader = MockLibraryLoader::new();
        loader.expect_load().returning(move |_| {
            let mut lib = MockLoadedLibrary::new();
            lib.expect_identity().return_const(1usize);
            let (gate, entered) = (gate.clone(), entered.clone());
            lib.expect_call_json().returning(move |_, _| {
                entered.fetch_add(1, Ordering::SeqCst);
                gate.wait();
                Ok("\"\"".into())
            });
            Ok(Box::new(lib))
        });
        let poll = meta(3, Duration::from_secs(60));
        let spec = ConfigSourceSpec {
            provider: "p".into(),
            name: "s".into(),
            root: None,
            data: format!(
                "{{{{ ('{}' | read) ~ plugin('/gate/libgate.so', 'wait') }}}}",
                state.display()
            ),
            result: ResultSpec::Fields(vec![("v".into(), "{{ data }}".into())]),
            recorded: crate::shape::decorators::Recorded {
                invalidation: poll.invalidation,
                failback: poll.failback,
                budget: poll.budget,
                timeout: poll.timeout,
                decay: None,
                nearest: false,
            },
        };
        let vars: Arc<HashMap<String, String>> = Arc::new(std::env::vars().collect());
        let plugins = Arc::new(crate::engine::PluginSet::new(vec![], Arc::new(loader)));
        let probe = crate::provider::config_source::load_probe(&vars, &plugins);
        let source = ConfigSource::new(spec, vars, plugins, Arc::new(NoReads), &probe).unwrap();
        let mut registry = ProviderRegistry::new();
        registry.register_config_sources("p", vec![source]).unwrap();
        Arc::new(registry)
    }

    /// A file a config Source's lines read is an input of the record its run
    /// makes, stamped before the read: rewritten after the run read it, the
    /// record the run stores is invalid at once, the next read runs it again,
    /// and a read that finds the change while the run is still going does not
    /// take that run, which read the file before it changed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_run_stamps_what_its_lines_read_before_it_reads_it() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let old = std::time::SystemTime::now() - Duration::from_secs(10);
        let write = |body: &str| {
            std::fs::write(&state, body).unwrap();
            // Long since written: no stamp of it is racy.
            std::fs::File::options()
                .write(true)
                .open(&state)
                .unwrap()
                .set_modified(old)
                .unwrap();
        };
        write("1");
        let (gate, entered) = (Arc::new(Gate::default()), Arc::new(AtomicU32::new(0)));
        let registry = gated_config_source(&state, gate.clone(), entered.clone());
        let cache = Arc::new(Cache::new());
        let key = crate::provider::source_call::instance_key("p", "s", None);
        let start = |want| registry.runs().start(want, &registry, &cache, &key);
        let stored = || {
            cache
                .get_source("p", None, "s")
                .and_then(|s| s.fields.get("v").cloned())
        };

        let until_entered = |n: u32| {
            let entered = entered.clone();
            async move {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                while entered.load(Ordering::SeqCst) < n {
                    assert!(std::time::Instant::now() < deadline, "never {n} reads");
                    tokio::task::yield_now().await;
                }
            }
        };

        let first = start(Want::Cold).unwrap();
        until_entered(1).await;
        // The run has read the file and waits at the gate.
        write("22");
        gate.open();
        assert_eq!(first.outcome().await, Outcome::Stored);
        assert_eq!(stored(), Some(Value::String("1".into())));
        assert_eq!(
            cache.record_state("p", None, "s"),
            crate::cache::RecordState::Invalid,
            "a Source that only polls is checked for the file its lines read"
        );

        gate.close();
        let second = start(Want::Changed).unwrap();
        until_entered(2).await;
        write("333");
        let third = start(Want::Changed).unwrap();
        assert_eq!(
            registry.runs().callers(&key),
            Some(2),
            "the run in flight read the file before it changed: one begins in its place"
        );
        gate.open();
        assert_eq!(second.outcome().await, Outcome::Stored);
        assert_eq!(third.outcome().await, Outcome::Stored);
        assert_eq!(entered.load(Ordering::SeqCst), 3);
        assert_eq!(stored(), Some(Value::String("333".into())));
        assert_eq!(
            cache.record_state("p", None, "s"),
            crate::cache::RecordState::Valid,
            "as its run read it"
        );
    }

    /// A caller that stops waiting takes nothing with it: the run records
    /// itself.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_run_records_itself_when_no_one_waits() {
        let f = fixture(3, Duration::from_secs(60));
        drop(f.start(Want::Cold).unwrap());
        f.until_runs(1).await;
        f.gate.open();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while f.stored().is_none() {
            assert!(std::time::Instant::now() < deadline, "never stored");
            tokio::task::yield_now().await;
        }
    }

    /// A wait whose deadline has passed is answered on its first poll, with
    /// what the run has come to by then: nothing while it is held, its outcome
    /// once it has ended. It waits on no timer: tokio's turns in whole
    /// milliseconds, and would hold it until the next turn. One worker, so
    /// nothing turns the timer between the deadline and the poll.
    #[tokio::test]
    async fn a_wait_already_over_is_answered_at_once() {
        use std::task::Poll;
        let f = fixture(3, Duration::from_secs(60));
        let run = f.start(Want::Cold).unwrap();
        let over = tokio::time::Instant::now();
        let held = tokio_test::task::spawn(run.outcome_by(over)).poll();
        f.gate.open();
        assert_eq!(held, Poll::Ready(None), "held: nothing, at once");
        assert_eq!(run.outcome().await, Outcome::Stored);
        assert_eq!(
            tokio_test::task::spawn(run.outcome_by(over)).poll(),
            Poll::Ready(Some(Outcome::Stored)),
            "ended: its outcome, at once"
        );
    }

    /// A failure is the run's outcome, counts toward the failback and leaves
    /// the last good value; in backoff only `--force` runs it, and a queued
    /// run the backoff holds back is dropped.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_failback_holds_back_all_but_a_forced_run() {
        let f = fixture(1, Duration::from_secs(60));
        f.gate.open();
        assert_eq!(
            f.start(Want::Cold).unwrap().outcome().await,
            Outcome::Stored
        );
        f.fail.store(true, Ordering::SeqCst);
        assert_eq!(
            f.start(Want::Fresh).unwrap().outcome().await,
            Outcome::Failed {
                message: "down".into(),
                fault: false
            }
        );
        assert_eq!(
            f.stored(),
            Some(Value::Int(1)),
            "the last good value stands"
        );
        assert!(f.start(Want::Any).is_none(), "in backoff");
        assert!(f.start(Want::Fresh).is_none(), "in backoff");
        assert!(matches!(
            f.start(Want::Forced).unwrap().outcome().await,
            Outcome::Failed { .. }
        ));
        assert_eq!(f.runs.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_run_past_its_timeout_fails_and_its_thread_is_left_to_finish() {
        let f = fixture(3, Duration::from_secs(1));
        let run = f.start(Want::Cold).unwrap();
        // The run's thread waits on the gate; on the paused clock only the
        // timeout can end the run.
        f.until_runs(1).await;
        tokio::time::advance(Duration::from_secs(2)).await;
        assert_eq!(
            run.outcome().await,
            Outcome::Failed {
                message: "timed out after 1s".into(),
                fault: false
            }
        );
        assert!(f.registry.runs().in_flight().is_empty());
        f.gate.open();
    }

    /// A reload that removes or changes the Source drops the run in flight
    /// and what is queued behind it; one that reschedules it runs it again
    /// under the new registry, and the run's waiters wait on that one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reload_overtakes_the_run_in_flight() {
        let f = fixture(3, Duration::from_secs(60));
        let run = f.start(Want::Cold).unwrap();
        let queued = f.start(Want::Fresh).unwrap();
        f.until_runs(1).await;
        let after = f.reloaded();
        f.registry
            .runs()
            .supersede(&f.key, Overtake::Gone, &after, &f.cache);
        f.gate.open();
        assert_eq!(run.outcome().await, Outcome::Dropped);
        assert_eq!(queued.outcome().await, Outcome::Dropped);
        assert_eq!(f.stored(), None, "an overtaken run writes nothing");
        assert_eq!(f.runs.load(Ordering::SeqCst), 1, "and nothing runs again");

        let f = fixture(3, Duration::from_secs(60));
        let run = f.start(Want::Cold).unwrap();
        f.until_runs(1).await;
        f.registry
            .runs()
            .supersede(&f.key, Overtake::Again, &f.registry, &f.cache);
        f.gate.open();
        assert_eq!(
            run.outcome().await,
            Outcome::Stored,
            "the run that replaced it"
        );
        assert_eq!(
            tokio_test::task::spawn(run.outcome_by(tokio::time::Instant::now())).poll(),
            std::task::Poll::Ready(Some(Outcome::Stored)),
            "and so, without waiting, for a wait already over"
        );
        assert_eq!(f.stored(), Some(Value::Int(2)), "the second run's record");
    }

    /// A caller of the Source now in force that arrives while the reload's
    /// overtaken run is still under way does not take that run (it would get
    /// nothing): it waits for one of its own, queued behind it, which runs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_caller_after_a_reload_waits_for_a_run_of_its_own() {
        let f = fixture(3, Duration::from_secs(60));
        let run = f.start(Want::Cold).unwrap();
        f.until_runs(1).await;
        let after = f.reloaded();
        f.registry
            .runs()
            .supersede(&f.key, Overtake::Gone, &after, &f.cache);
        let late = after
            .runs()
            .start(Want::Cold, &after, &f.cache, &f.key)
            .expect("a run is queued for it");
        f.gate.open();
        assert_eq!(run.outcome().await, Outcome::Dropped);
        assert_eq!(late.outcome().await, Outcome::Stored);
        assert_eq!(f.stored(), Some(Value::Int(2)), "the run of its own stored");
    }

    /// Two runs whose threads would wait on each other: whichever asks second
    /// is told it would close the ring, and neither waits for ever. A run
    /// that asks for itself is told the same.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_wait_that_would_close_a_ring_is_refused() {
        let f = fixture(3, Duration::from_secs(60));
        let a = f.key.clone();
        let b = crate::provider::source_call::instance_key("p", "s", Some("/b"));
        let runs = Arc::clone(f.registry.runs());
        // Two runs in flight, as far as the table goes, whose threads are
        // these two.
        let table_run = |key: &Key| {
            lock(&runs.table).slots.insert(
                key.clone(),
                Slot {
                    running: Arc::default(),
                    queued: None,
                    overtaken: None,
                },
            );
            runs.run(key, Arc::clone(&lock(&runs.table).slots[key].running))
        };
        let (run_a, run_b) = (table_run(&a), table_run(&b));
        let on = |me: &Run, waits_on: Run| {
            let me = id(&me.state);
            std::thread::spawn(move || {
                let _here = Here::enter(me, Arc::default());
                waits_on.wait()
            })
        };
        let a_waits_b = on(&run_a, run_b.clone());
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !lock(&runs.table).waits.contains_key(&id(&run_a.state)) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(on(&run_b, run_a.clone()).join().unwrap(), Err(Cycle));
        assert_eq!(on(&run_a, run_a.clone()).join().unwrap(), Err(Cycle));
        // B ends; A, which waited on it, has its outcome.
        lock(&runs.table).slots.remove(&b);
        run_b.state.finish(End::Done(Outcome::Stored));
        assert_eq!(a_waits_b.join().unwrap(), Ok(Outcome::Stored));
        assert!(lock(&runs.table).waits.is_empty());
    }

    /// The daemon stopping drops a run's task before it ends: the run ends
    /// Dropped, so a blocking thread waiting on it is let go (a runtime's drop
    /// waits for its blocking threads).
    #[test]
    fn a_run_whose_task_is_dropped_ends_and_lets_its_waiters_go() {
        let f = fixture(3, Duration::from_secs(60));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let run = {
            let _enter = rt.enter();
            f.start(Want::Cold).unwrap()
        };
        let waiter = std::thread::spawn({
            let run = run.clone();
            move || run.wait()
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while f.runs.load(Ordering::SeqCst) < 1 {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        // Its thread is held by the gate; the runtime goes without waiting
        // for it.
        rt.shutdown_background();
        assert_eq!(waiter.join().unwrap(), Ok(Outcome::Dropped));
        assert!(f.registry.runs().in_flight().is_empty());
        f.gate.open();
    }
}
