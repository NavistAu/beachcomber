use beachcomber::cache::Cache;
use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    Source, SourceMetadata, SourceResult, Value,
};
use beachcomber::query::Instance;
use beachcomber::scheduler::{Scheduler, SchedulerMessage};
use beachcomber::watcher_registry::WatcherRegistry;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use crate::common::poll::poll_until_async;

/// A latch the test opens when it wants the in-flight execution to finish.
/// Holding it shut is what "an execution is still running" means here — no
/// guess about how long the work takes. It opens for good, or lets one
/// execution through at a time.
struct Gate {
    /// Open for good, and how many executions may pass meanwhile.
    state: std::sync::Mutex<(bool, u32)>,
    cv: std::sync::Condvar,
}

impl Gate {
    fn new() -> Self {
        Self {
            state: std::sync::Mutex::new((false, 0)),
            cv: std::sync::Condvar::new(),
        }
    }

    fn wait(&self) {
        let mut state = self.state.lock().unwrap();
        while !state.0 && state.1 == 0 {
            state = self.cv.wait(state).unwrap();
        }
        if !state.0 {
            state.1 -= 1;
        }
    }

    fn release(&self) {
        self.state.lock().unwrap().0 = true;
        self.cv.notify_all();
    }

    /// Let the execution waiting now (or the next to come) through, and no
    /// other.
    fn release_one(&self) {
        self.state.lock().unwrap().1 += 1;
        self.cv.notify_all();
    }
}

fn counter_source_meta() -> SourceMetadata {
    SourceMetadata {
        name: "main".into(),
        fields: vec![FieldSchema {
            name: "count".into(),
            field_type: FieldType::Int,
        }],
        invalidation: InvalidationStrategy::Poll {
            interval_secs: 60,
            count: 2,
        },
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 30,
        },
        decay: None,
        budget: beachcomber::scheduler::lifecycle::DEFAULT_BUDGET,
        timeout: beachcomber::scheduler::lifecycle::DEFAULT_TIMEOUT,
        nearest: false,
    }
}

struct CountingSourceImpl {
    counter: Arc<AtomicU32>,
    gate: Arc<Gate>,
}

impl Source for CountingSourceImpl {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(counter_source_meta)
    }

    fn execute(&self, _path: Option<&str>) -> Result<SourceResult, String> {
        let count = self.counter.fetch_add(1, Ordering::SeqCst);
        // Stand in for work of unknown length: the test decides when this
        // execution ends, so the window the deduplicated refreshes land in is
        // exact rather than guessed.
        self.gate.wait();
        let mut result = SourceResult::new();
        result.insert("count", Value::Int(count as i64));
        Ok(result)
    }
}

struct CountingProvider {
    counter: Arc<AtomicU32>,
    gate: Arc<Gate>,
}

impl CountingProvider {
    fn new(counter: Arc<AtomicU32>, gate: Arc<Gate>) -> Self {
        Self { counter, gate }
    }
}

impl Provider for CountingProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "counter".to_string(),
            sources: vec![counter_source_meta()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(CountingSourceImpl {
            counter: Arc::clone(&self.counter),
            gate: Arc::clone(&self.gate),
        })]
    }
}

#[tokio::test]
async fn rapid_refreshes_are_deduplicated() {
    let exec_count = Arc::new(AtomicU32::new(0));
    let gate = Arc::new(Gate::new());

    let cache = Arc::new(Cache::new());
    let mut registry = ProviderRegistry::new();
    registry
        .register(Box::new(CountingProvider::new(
            Arc::clone(&exec_count),
            Arc::clone(&gate),
        )))
        .expect("counting");
    let registry = Arc::new(registry);
    let config = Config::default();

    let (handle, mut scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
    scheduler.use_polling_watcher(Duration::from_millis(50));
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    // Send 10 rapid refreshes
    for _ in 0..10 {
        handle
            .send(SchedulerMessage::Refresh {
                provider: "counter".to_string(),
                instances: vec![Instance {
                    source: "main".to_string(),
                    root: None,
                }],
            })
            .await;
    }

    // All ten are now queued behind a FIFO channel; this round-trip proves every
    // one of them has been handled. The first started an execution that is
    // parked on the gate, so the other nine could only have taken the
    // already-in-flight path — no sleep needed to "make them overlap".
    let status = handle
        .get_status()
        .await
        .expect("status should be available");
    assert!(
        status.in_flight.iter().any(|k| k == "counter:main"),
        "counter.main should still be in flight; got {:?}",
        status.in_flight
    );

    // Let the held execution finish, then wait for the scheduler to drain: once
    // in_flight is empty (including the queued rerun's own bookkeeping), the
    // execution count is final.
    gate.release();
    poll_until_async(
        "scheduler in_flight",
        "empty",
        Duration::from_secs(5),
        Duration::from_millis(5),
        async || {
            handle
                .get_status()
                .await
                .expect("status available")
                .in_flight
        },
        |f| f.is_empty(),
    )
    .await;

    let count = exec_count.load(Ordering::SeqCst);
    // The first refresh started the execution parked on the gate; the other
    // nine collapsed into one queued rerun, which ran once the gate opened.
    // Exactly two executions: not ten, and not one.
    assert_eq!(
        count, 2,
        "10 refreshes against an in-flight execution should run exactly twice, got {count}"
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}

// ── one run path for an instance, whoever starts it ─────────────────────────

/// One request on a connection of its own.
async fn ask(sock: std::path::PathBuf, request: serde_json::Value) -> serde_json::Value {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut stream = tokio::net::UnixStream::connect(&sock).await.unwrap();
    let mut line = request.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes()).await.unwrap();
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer).await.unwrap();
    serde_json::from_str(&answer).expect("a JSON answer")
}

/// Yield to the runtime until `done` holds: for what a blocking thread does,
/// which the runtime does not see, and for a paused clock, under which a sleep
/// would never wake. Bounded in real time.
async fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "never: {what}");
        tokio::task::yield_now().await;
    }
}

/// The runs in flight, as the scheduler's status lists them.
async fn in_flight(handle: &beachcomber::scheduler::SchedulerHandle) -> Vec<String> {
    handle.get_status().await.expect("status").in_flight
}

async fn until_settled(handle: &beachcomber::scheduler::SchedulerHandle) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !in_flight(handle).await.is_empty() {
        assert!(std::time::Instant::now() < deadline, "runs never settled");
        tokio::task::yield_now().await;
    }
}

/// A Source that counts its runs and holds each one until the test opens
/// its gate, polled every `poll` seconds.
struct HeldSource {
    meta: SourceMetadata,
    runs: Arc<AtomicU32>,
    gate: Arc<Gate>,
}

impl Source for HeldSource {
    fn metadata(&self) -> &SourceMetadata {
        &self.meta
    }

    fn execute(&self, _path: Option<&str>) -> Result<SourceResult, String> {
        let run = self.runs.fetch_add(1, Ordering::SeqCst) + 1;
        self.gate.wait();
        let mut result = SourceResult::new();
        result.insert("count", Value::Int(i64::from(run)));
        Ok(result)
    }
}

struct HeldProvider {
    meta: SourceMetadata,
    runs: Arc<AtomicU32>,
    gate: Arc<Gate>,
}

impl Provider for HeldProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "held".to_string(),
            sources: vec![self.meta.clone()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(HeldSource {
            meta: self.meta.clone(),
            runs: Arc::clone(&self.runs),
            gate: Arc::clone(&self.gate),
        })]
    }
}

/// A server and a scheduler sharing a registry of the one held Source. The
/// gate opens when it is dropped, so a failing test does not leave a run's
/// thread held for the runtime to wait on.
struct Held {
    sock: std::path::PathBuf,
    _dir: tempfile::TempDir,
    handle: beachcomber::scheduler::SchedulerHandle,
    heartbeat: Arc<std::sync::atomic::AtomicU64>,
    runs: Arc<AtomicU32>,
    gate: Arc<Gate>,
}

impl Drop for Held {
    fn drop(&mut self) {
        self.gate.release();
    }
}

fn held(poll_secs: u64) -> Held {
    let runs = Arc::new(AtomicU32::new(0));
    let gate = Arc::new(Gate::new());
    let meta = SourceMetadata {
        invalidation: InvalidationStrategy::Poll {
            interval_secs: poll_secs,
            count: 60,
        },
        timeout: Duration::from_secs(600),
        ..counter_source_meta()
    };
    let mut registry = ProviderRegistry::new();
    registry
        .register(Box::new(HeldProvider {
            meta,
            runs: Arc::clone(&runs),
            gate: Arc::clone(&gate),
        }))
        .expect("held");
    let registry = Arc::new(registry);
    let watchers = Arc::new(WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let (handle, mut scheduler) = Scheduler::new(
        cache.clone(),
        registry.clone(),
        Config::default(),
        watchers.clone(),
    );
    scheduler.use_polling_watcher(Duration::from_millis(50));
    let heartbeat = scheduler.heartbeat();
    tokio::spawn(scheduler.run());
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("s");
    beachcomber::server::Server::new(
        sock.clone(),
        cache,
        registry,
        Some(handle.clone()),
        watchers,
    )
    .spawn()
    .unwrap();
    Held {
        sock,
        _dir: dir,
        handle,
        heartbeat,
        runs,
        gate,
    }
}

fn held_instance() -> Vec<Instance> {
    vec![Instance::new("main", None)]
}

/// A read finds the instance cold and runs it. While that run is held, the
/// scheduler's first demand of the instance and then its poll arrive: both
/// take the run in flight. One run, whoever asked.
///
/// The poll is a real one: its interval is a second, the shortest there is,
/// and the lifecycle's clock is the system's, so this waits that second out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_a_demand_and_a_poll_of_one_instance_run_it_once() {
    let d = held(1);
    let read = tokio::spawn(ask(
        d.sock.clone(),
        serde_json::json!({"op": "get", "key": "held.count"}),
    ));
    until("the read's run to begin", || {
        d.runs.load(Ordering::SeqCst) == 1
    })
    .await;

    d.handle
        .send(SchedulerMessage::QueryActivity {
            provider: "held".into(),
            instances: held_instance(),
        })
        .await;
    let demanded = std::time::Instant::now();
    d.handle.get_status().await.expect("the demand was handled");
    // The poll is due a second after the demand; the first tick after that
    // runs it. Two ticks past the second, and a round trip behind them.
    let beat = d.heartbeat.load(Ordering::Relaxed);
    let deadline = demanded + Duration::from_secs(10);
    while demanded.elapsed() < Duration::from_secs(1)
        || d.heartbeat.load(Ordering::Relaxed) < beat + 2
    {
        assert!(std::time::Instant::now() < deadline, "the ticks stopped");
        tokio::task::yield_now().await;
    }
    d.handle.get_status().await.expect("the poll was handled");
    assert_eq!(
        d.runs.load(Ordering::SeqCst),
        1,
        "the demand and the poll took the read's run"
    );

    d.gate.release();
    assert_eq!(read.await.unwrap()["data"], serde_json::json!(1));
    until_settled(&d.handle).await;
    assert_eq!(d.runs.load(Ordering::SeqCst), 1);
}

/// A refresh that arrives while a read's run is under way waits behind it:
/// three of them make one more run, after it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refresh_during_a_reads_run_queues_one_rerun() {
    let d = held(60);
    let read = tokio::spawn(ask(
        d.sock.clone(),
        serde_json::json!({"op": "get", "key": "held.count"}),
    ));
    until("the read's run to begin", || {
        d.runs.load(Ordering::SeqCst) == 1
    })
    .await;
    for _ in 0..3 {
        d.handle
            .send(SchedulerMessage::Refresh {
                provider: "held".into(),
                instances: held_instance(),
            })
            .await;
    }
    // The channel is FIFO: once this answers, the refreshes were handled.
    assert_eq!(
        in_flight(&d.handle).await,
        ["held:main"],
        "one run, the read's"
    );
    assert_eq!(d.runs.load(Ordering::SeqCst), 1, "the refreshes wait");

    // The read's run alone: the rerun waits until the read has answered,
    // which it does from the record, where the rerun would have put its 2.
    d.gate.release_one();
    assert_eq!(read.await.unwrap()["data"], serde_json::json!(1));
    d.gate.release();
    until_settled(&d.handle).await;
    assert_eq!(
        d.runs.load(Ordering::SeqCst),
        2,
        "three refreshes, one rerun"
    );
}

/// A config Source whose `data` line touches `<dir>/started`, waits for
/// `<dir>/go`, then answers 1, polled every `poll`.
fn gated_source(dir: &std::path::Path, poll: &str) -> String {
    format!(
        "[daemon.providers.demo.sources.s]\n\
         data = \"{{{{ exec('touch {d}/started; while [ ! -f {d}/go ]; do sleep 0.01; done; echo 1') | trim }}}}\"\n\
         result.n = \"{{{{ data }}}}\"\n\
         return = \"{{{{ result | poll('{poll}') }}}}\"\n",
        d = dir.display()
    )
}

/// A reload that reschedules a Source overtakes the run a read started: the
/// run's result is not written (it carries the old schedule), the instance
/// runs again under the Source now in force, and the read waiting on it
/// answers from that run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reload_supersedes_a_run_a_read_started() {
    use beachcomber::provider::registry::RegistryHandle;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    let config = Config::from_toml(&gated_source(&dir, "10m"));
    let watchers = Arc::new(WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let before = ProviderRegistry::with_config(&config, &std::env::vars().collect())
        .unwrap()
        .share(&cache);
    let in_force = RegistryHandle::new(before.clone());
    let (handle, mut scheduler) =
        Scheduler::new(cache.clone(), before.clone(), config, watchers.clone());
    scheduler.use_polling_watcher(Duration::from_millis(50));
    tokio::spawn(scheduler.run());
    let sock = dir.join("s");
    beachcomber::server::Server::new(
        sock.clone(),
        cache.clone(),
        in_force.clone(),
        Some(handle.clone()),
        watchers,
    )
    .spawn()
    .unwrap();

    let read = tokio::spawn(ask(
        sock.clone(),
        serde_json::json!({"op": "get", "key": "demo.n"}),
    ));
    until("the read's run to begin", || dir.join("started").exists()).await;

    let after = ProviderRegistry::with_config(
        &Config::from_toml(&gated_source(&dir, "11m")),
        &std::env::vars().collect(),
    )
    .unwrap()
    .adopt(&before, &cache);
    handle
        .send(SchedulerMessage::Reload {
            next: after,
            registry: in_force,
        })
        .await;
    handle.get_status().await.expect("the reload was handled");

    std::fs::write(dir.join("go"), "").unwrap();
    assert_eq!(read.await.unwrap()["data"], serde_json::json!("1"));
    until_settled(&handle).await;
    let schedule = ask(
        sock.clone(),
        serde_json::json!({"op": "get", "key": "demo:cache.n"}),
    )
    .await;
    assert_eq!(
        schedule["data"]["poll"]["interval"],
        serde_json::json!("660s"),
        "the record is the run under the new Source's, not the overtaken one's: {schedule}"
    );
}

/// Two Sources that read each other's provider (`comb.q` in one, `comb.p` in
/// the other), both cold and run at once by two reads: each run waits for
/// the other's only until that would close the ring, and the run that would
/// close it finds the other's last value (nothing yet) instead. Both reads
/// answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ring_of_reads_between_runs_finds_the_last_value_and_waits_on_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    // `comb[...]` under a computed name is read when the render reaches it,
    // after the gate (a name written out is read before the line renders).
    let side = |provider: &str, source: &str, other: &str| {
        format!(
            "[daemon.providers.{provider}.sources.{source}]\n\
             data = \"{{{{ exec('touch {d}/{source}; while [ ! -f {d}/go ]; do sleep 0.01; done') ~ \
             '{source}<' ~ (comb['{other}' ~ ''].v if comb['{other}' ~ ''] else '-') ~ '>' }}}}\"\n\
             result.v = \"{{{{ data }}}}\"\n\
             return = \"{{{{ result | timeout('3s') }}}}\"\n",
            d = dir.display()
        )
    };
    let config = Config::from_toml(&format!("{}{}", side("p", "a", "q"), side("q", "b", "p")));
    let watchers = Arc::new(WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = ProviderRegistry::with_config(&config, &std::env::vars().collect())
        .unwrap()
        .share(&cache);
    let sock = dir.join("s");
    beachcomber::server::Server::new(sock.clone(), cache, registry, None, watchers)
        .spawn()
        .unwrap();

    let p = tokio::spawn(ask(
        sock.clone(),
        serde_json::json!({"op": "get", "key": "p.v"}),
    ));
    let q = tokio::spawn(ask(
        sock.clone(),
        serde_json::json!({"op": "get", "key": "q.v"}),
    ));
    until("both runs to begin", || {
        dir.join("a").exists() && dir.join("b").exists()
    })
    .await;
    std::fs::write(dir.join("go"), "").unwrap();
    let started = std::time::Instant::now();
    let (p, q) = (
        p.await.unwrap()["data"].clone(),
        q.await.unwrap()["data"].clone(),
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the reads waited {:?}: a run waited on itself round the ring",
        started.elapsed()
    );
    let pair = (p.as_str().unwrap_or("?"), q.as_str().unwrap_or("?"));
    assert!(
        pair == ("a<b<->>", "b<->") || pair == ("a<->", "b<a<->>"),
        "one run read the other's last value (none), the other waited for it: {pair:?}"
    );
}
