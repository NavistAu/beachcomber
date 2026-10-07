use beachcomber::cache::Cache;
use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use beachcomber::query::SourceDemand;
use beachcomber::scheduler::{Scheduler, SchedulerMessage};
use beachcomber::watcher_registry::WatcherRegistry;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use crate::common::poll::poll_until_async;

/// A latch the test opens when it wants the in-flight execution to finish.
/// Holding it shut is what "an execution is still running" means here — no
/// guess about how long the work takes.
struct Gate {
    released: std::sync::Mutex<bool>,
    cv: std::sync::Condvar,
}

impl Gate {
    fn new() -> Self {
        Self {
            released: std::sync::Mutex::new(false),
            cv: std::sync::Condvar::new(),
        }
    }

    fn wait(&self) {
        let mut released = self.released.lock().unwrap();
        while !*released {
            released = self.cv.wait(released).unwrap();
        }
    }

    fn release(&self) {
        *self.released.lock().unwrap() = true;
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
        scope: SourceScope::Global,
        invalidation: InvalidationStrategy::Poll {
            interval_secs: 60,
            count: 2,
        },
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 30,
        },
        fsevents_reinstate: false,
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

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        let count = self.counter.fetch_add(1, Ordering::SeqCst);
        // Stand in for work of unknown length: the test decides when this
        // execution ends, so the window the deduplicated refreshes land in is
        // exact rather than guessed.
        self.gate.wait();
        let mut result = SourceResult::new();
        result.insert("count", Value::Int(count as i64));
        result
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
                path: None,
                demand: SourceDemand::All,
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
