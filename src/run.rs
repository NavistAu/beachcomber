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

use crate::cache::Cache;
use crate::provider::registry::ProviderRegistry;
use crate::provider::source_call::{in_failback_backoff, ran};
use crate::provider::{Source, SourceResult};
use crate::scheduler::lifecycle::Key;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use tracing::{debug, warn};

/// What a caller needs of a run of an instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    /// A record: the run in flight, else a new one, unless the instance has
    /// stored one by now. A cold read, a Source's call, an instance's first
    /// demand.
    Cold,
    /// The run in flight, else a new one. A poll.
    Any,
    /// A run that begins after now: the one queued behind the run in flight,
    /// if there is one. A file event, a refresh, a read of a Source that runs
    /// on every read.
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
    end: Mutex<Option<End>>,
    ended: Condvar,
    notify: tokio::sync::Notify,
}

impl Default for State {
    fn default() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            end: Mutex::default(),
            ended: Condvar::default(),
            notify: tokio::sync::Notify::default(),
        }
    }
}

#[derive(Clone)]
enum End {
    Done(Outcome),
    /// Overtaken by a reload: the run that replaces it ends for them.
    Moved(Arc<State>),
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl State {
    fn finish(&self, end: End) {
        *lock(&self.end) = Some(end);
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

thread_local! {
    /// The run executing on this thread, if any: what a wait from it is.
    static HERE: RefCell<Option<RunId>> = const { RefCell::new(None) };
}

/// A run executes on this thread for as long as this is held.
struct Here(Option<RunId>);

impl Here {
    fn enter(run: RunId) -> Self {
        Here(HERE.with(|h| h.replace(Some(run))))
    }
}

impl Drop for Here {
    fn drop(&mut self) {
        HERE.with(|h| *h.borrow_mut() = self.0.take());
    }
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

    /// [`Self::outcome`], from a blocking thread. From the thread of a run,
    /// [`Cycle`] when waiting would close a ring: the run waited on is
    /// waiting, however far round, on this one (or is this one).
    pub fn wait(&self) -> Result<Outcome, Cycle> {
        let me = HERE.with(|h| *h.borrow());
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
            let ran = tokio::time::timeout(
                timeout,
                tokio::task::spawn_blocking(move || {
                    let _registry = keep;
                    let _here = Here::enter(here);
                    ran(|| source.execute(dir.as_deref()))
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
        result: Option<Result<SourceResult, (String, bool)>>,
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
    fn finish(mut self, result: Result<SourceResult, (String, bool)>) {
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
fn record(
    registry: &ProviderRegistry,
    cache: &Cache,
    key: &Key,
    result: Result<SourceResult, (String, bool)>,
) -> Outcome {
    let (provider, source_name, root) = (&key.provider, &key.source, key.path.as_deref());
    let Some(source) = registry.source(provider, source_name) else {
        return Outcome::Dropped;
    };
    let meta = source.metadata();
    registry
        .unless_retired(provider, source_name, || match result {
            Ok(result) => {
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
                    crate::cache::Schedule::of(meta),
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
                    .record_failure();
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
        FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
        SourceMetadata, Value,
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
            fields: vec![FieldSchema {
                name: "n".into(),
                field_type: FieldType::Int,
            }],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 60,
                count: 2,
            },
            failback: FailbackConfig {
                reattempts: failback,
                interval_secs: 3600,
            },
            fsevents_reinstate: false,
            decay: None,
            budget_execute: crate::scheduler::lifecycle::DEFAULT_BUDGET_EXECUTE,
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

    impl Provider for P {
        fn metadata(&self) -> ProviderMetadata {
            ProviderMetadata {
                name: "p".into(),
                sources: vec![self.0.clone()],
            }
        }
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
        let runs = Arc::new(AtomicU32::new(0));
        let gate = Arc::new(Gate::default());
        let fail = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut registry = ProviderRegistry::new();
        registry
            .register(Box::new(P(
                meta(failback, timeout),
                runs.clone(),
                gate.clone(),
                fail.clone(),
            )))
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
            next.register(Box::new(P(
                meta(3, Duration::from_secs(60)),
                self.runs.clone(),
                self.gate.clone(),
                self.fail.clone(),
            )))
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
                let _here = Here::enter(me);
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
