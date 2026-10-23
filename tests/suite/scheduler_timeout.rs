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
use std::time::Duration;

use crate::common::poll::poll_until_async;

/// A latch the test opens when it is done. `execute()` parks on it instead of
/// sleeping a guessed duration: "still running when the timeout fires" becomes
/// a fact the test controls rather than a race against wall-clock time.
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

fn slow_source_meta() -> SourceMetadata {
    SourceMetadata {
        name: "main".into(),
        fields: vec![FieldSchema {
            name: "value".into(),
            field_type: FieldType::String,
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
        decay: None,
        budget_execute: beachcomber::scheduler::lifecycle::DEFAULT_BUDGET_EXECUTE,
    }
}

struct SlowSourceImpl {
    gate: Arc<Gate>,
}

impl Source for SlowSourceImpl {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(slow_source_meta)
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        // Block until the test opens the gate. It stays shut across the whole
        // timeout window, so the scheduler's timeout arm is guaranteed to be
        // the one that fires; opening it afterwards lets this blocking-pool
        // thread retire immediately instead of holding up runtime shutdown.
        self.gate.wait();
        let mut result = SourceResult::new();
        result.insert("value", Value::String("done".to_string()));
        result
    }
}

struct SlowProvider {
    gate: Arc<Gate>,
}

impl Provider for SlowProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "slow".to_string(),
            sources: vec![slow_source_meta()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(SlowSourceImpl {
            gate: Arc::clone(&self.gate),
        })]
    }
}

/// Uses the tokio mock clock so no wall-clock time passes at all.
///
/// The scheduler wraps the spawn_blocking execution with `tokio::time::timeout(1s)`.
/// The provider parks on a gate the test holds shut, so the only thing that can
/// finish that execution is the timeout; `advance(1.5s)` fires it on the mock
/// clock. `in_flight` is cleared by the same task that handles the timeout
/// result, so waiting for it to empty is a positive observation that the
/// `Err(_)` timeout arm ran — which is what makes the "cache is still empty"
/// assertion below mean "timed out" rather than "hasn't got there yet".
#[tokio::test(start_paused = true)]
async fn slow_provider_times_out() {
    let gate = Arc::new(Gate::new());

    let cache = Arc::new(Cache::new());
    let mut registry = ProviderRegistry::new();
    registry
        .register(Box::new(SlowProvider {
            gate: Arc::clone(&gate),
        }))
        .expect("slow");
    let registry = Arc::new(registry);

    let config = Config::from_toml("[daemon]\nprovider_timeout_secs = 1\n");

    let (handle, mut scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
    scheduler.use_polling_watcher(Duration::from_millis(50));
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    handle
        .send(SchedulerMessage::Refresh {
            provider: "slow".to_string(),
            path: None,
            demand: SourceDemand::All,
        })
        .await;

    // `execute_source` inserts into `in_flight` while handling the Refresh, and
    // the channel is FIFO — so this round-trip both proves the refresh was
    // dispatched and shows the execution is live and parked on the gate.
    let status = handle
        .get_status()
        .await
        .expect("status should be available");
    assert!(
        status.in_flight.iter().any(|k| k == "slow:main"),
        "slow.main should be in flight while the gate is held; got {:?}",
        status.in_flight
    );

    // Let the task spawned for this execution register its `timeout` on the
    // mock clock: `advance` only fires timers that already exist.
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }

    // Advance the mock clock past the 1 s timeout. The tokio::time::timeout in
    // the scheduler fires; the blocking thread stays parked on the gate and its
    // JoinHandle result is discarded via the Err(_) timeout arm.
    tokio::time::advance(Duration::from_millis(1500)).await;

    // The timeout arm clears in_flight; the provider itself cannot, since the
    // gate is still shut — so an empty in_flight proves the Err(_) arm ran.
    // This waits by yielding rather than by `tokio::time::sleep`: with a
    // blocking task outstanding the paused clock does not auto-advance, so a
    // sleep-based poll would never wake.
    let mut in_flight = status.in_flight;
    for _ in 0..1000 {
        tokio::task::yield_now().await;
        in_flight = handle
            .get_status()
            .await
            .expect("status available")
            .in_flight;
        if in_flight.is_empty() {
            break;
        }
    }
    assert!(
        in_flight.is_empty(),
        "the timeout arm should have cleared in_flight; got {in_flight:?}"
    );

    // Cache should NOT have the value (timed out)
    assert!(
        cache.get_entry("slow", None).is_none(),
        "Timed-out provider should not populate cache"
    );

    // Let the parked blocking-pool thread retire before the runtime is torn down.
    gate.release();

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}

#[tokio::test]
async fn fast_provider_completes_within_timeout() {
    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());

    let config = Config::from_toml("[daemon]\nprovider_timeout_secs = 5\n");

    let (handle, mut scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
    scheduler.use_polling_watcher(Duration::from_millis(50));
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    handle
        .send(SchedulerMessage::Refresh {
            provider: "hostname".to_string(),
            path: None,
            demand: SourceDemand::All,
        })
        .await;

    let entry = poll_until_async(
        "hostname cache entry",
        "Some(_)",
        Duration::from_secs(5),
        Duration::from_millis(10),
        async || cache.get_entry("hostname", None),
        |e| e.is_some(),
    )
    .await;
    assert!(
        entry.is_some(),
        "Fast provider should complete within timeout"
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}
