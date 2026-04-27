//! End-to-end tests for the cache lifecycle.
//! Unit tests for the registry itself live in src/scheduler/lifecycle.rs.
//! These exercise the full scheduler + cache + a fake provider.

use beachcomber::cache::Cache;
use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use beachcomber::scheduler::{Scheduler, SchedulerHandle, SchedulerMessage};
use beachcomber::watcher_registry::WatcherRegistry;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

static POLL_COUNT: AtomicU32 = AtomicU32::new(0);

fn lc_counter_source_meta() -> SourceMetadata {
    SourceMetadata {
        name: "main".into(),
        fields: vec![FieldSchema {
            name: "value".into(),
            field_type: FieldType::Int,
        }],
        scope: SourceScope::Global,
        invalidation: InvalidationStrategy::Poll { interval_secs: 5 },
        keep_alive: KeepAlive::Polls(2),
        failback: FailbackConfig { reattempts: 3, interval_secs: 30 },
        fsevents_reinstate: false,
    }
}

struct LcCounterSourceImpl;

impl Source for LcCounterSourceImpl {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(lc_counter_source_meta)
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        let count = POLL_COUNT.fetch_add(1, Ordering::SeqCst);
        let mut result = SourceResult::new();
        result.insert("value", Value::Int(count as i64));
        result
    }
}

/// A simple global provider that increments a counter on each execute.
struct CountingGlobalProvider;

impl Provider for CountingGlobalProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "lc_counter".to_string(),
            sources: vec![lc_counter_source_meta()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(LcCounterSourceImpl)]
    }
}

/// Verifies that a cold cache miss via QueryActivity immediately triggers provider execution
/// and the cache entry is populated.
#[tokio::test]
async fn integration_cold_miss_populates_cache_and_enters_active() {
    POLL_COUNT.store(0, Ordering::SeqCst);

    let cache = Arc::new(Cache::new());
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(CountingGlobalProvider)).expect("lc_counter");
    let registry = Arc::new(registry);
    let config = Config::default();

    let (handle, scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    // Before any query, the cache should be empty.
    assert!(
        cache.get_entry("lc_counter", None).is_none(),
        "cache should be cold before first query"
    );

    // Signal demand — this should trigger inline execution.
    handle
        .send(SchedulerMessage::QueryActivity {
            provider: "lc_counter".to_string(),
            path: None,
        })
        .await;

    // Allow time for async execution to complete.
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    assert!(
        cache.get_entry("lc_counter", None).is_some(),
        "cache should be populated after cold miss QueryActivity"
    );
    assert!(
        POLL_COUNT.load(Ordering::SeqCst) >= 1,
        "provider should have executed at least once"
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}

/// Verifies that the scheduler status reports a poll timer for an active entry.
#[tokio::test]
async fn integration_active_entry_appears_in_status() {
    let cache = Arc::new(Cache::new());
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(CountingGlobalProvider)).expect("lc_counter");
    let registry = Arc::new(registry);
    let config = Config::default();

    let (handle, scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    handle
        .send(SchedulerMessage::QueryActivity {
            provider: "lc_counter".to_string(),
            path: None,
        })
        .await;

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let status = handle
        .get_status()
        .await
        .expect("status should be available");

    // The entry should appear in poll_timers (it's Active with a 5s poll interval).
    assert!(
        status
            .poll_timers
            .iter()
            .any(|t| t.provider == "lc_counter"),
        "lc_counter should appear in poll_timers when Active; got {:?}",
        status.poll_timers
    );

    // Active entries appear in demand, not lifecycle (decay).
    assert!(
        status.demand.iter().any(|d| d.provider == "lc_counter"),
        "lc_counter should appear in demand when Active; got {:?}",
        status.demand
    );
    assert!(
        !status.lifecycle.iter().any(|b| b.provider == "lc_counter"),
        "lc_counter should NOT be in lifecycle when Active; got {:?}",
        status.lifecycle
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}

/// Helper: start a scheduler and register the counting provider.
async fn setup_lifecycle_scheduler() -> (Arc<Cache>, SchedulerHandle, tokio::task::JoinHandle<()>) {
    let cache = Arc::new(Cache::new());
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(CountingGlobalProvider)).expect("lc_counter");
    let registry = Arc::new(registry);
    let config = Config::default();

    let (handle, scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
    let task = tokio::spawn(async move { scheduler.run().await });
    (cache, handle, task)
}

/// Verifies that the scheduler reports decay=0 for an Active cache entry.
#[tokio::test]
async fn integration_status_response_reports_decay_level() {
    let (cache, handle, sched_task) = setup_lifecycle_scheduler().await;

    // Trigger demand so the entry enters Active state.
    handle
        .send(SchedulerMessage::QueryActivity {
            provider: "lc_counter".to_string(),
            path: None,
        })
        .await;

    // Allow async execution to complete and lifecycle to stabilise.
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    assert!(
        cache.get_entry("lc_counter", None).is_some(),
        "cache should be warm before status check"
    );

    // Fetch the lifecycle snapshots from the scheduler.
    let snapshots = handle.get_lifecycle_snapshots().await;

    // Key is (provider, path, source_name)
    let key = ("lc_counter".to_string(), None::<String>, "main".to_string());
    let decay_level = snapshots.get(&key).map(|s| s.decay);

    assert_eq!(
        decay_level,
        Some(0),
        "lc_counter should have decay=0 (Active) shortly after first query; got {:?}",
        decay_level
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}

/// Verifies that repeated QueryActivity calls keep the entry warm and don't re-execute
/// if the cache is already populated.
#[tokio::test]
async fn integration_repeated_queries_keep_data_warm() {
    POLL_COUNT.store(0, Ordering::SeqCst);

    let cache = Arc::new(Cache::new());
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(CountingGlobalProvider)).expect("lc_counter");
    let registry = Arc::new(registry);
    let config = Config::default();

    let (handle, scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    // First query populates cache.
    handle
        .send(SchedulerMessage::QueryActivity {
            provider: "lc_counter".to_string(),
            path: None,
        })
        .await;

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let _entry_after_first = cache
        .get_entry("lc_counter", None)
        .expect("should have entry");
    let exec_after_first = POLL_COUNT.load(Ordering::SeqCst);

    // Subsequent queries should not re-execute (cache is warm, no inline miss).
    for _ in 0..5 {
        handle
            .send(SchedulerMessage::QueryActivity {
                provider: "lc_counter".to_string(),
                path: None,
            })
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let exec_after_repeated = POLL_COUNT.load(Ordering::SeqCst);
    let _entry_after_repeated = cache
        .get_entry("lc_counter", None)
        .expect("should still have entry");

    // Execution count should be the same (no re-execution triggered by repeat queries).
    assert_eq!(
        exec_after_first, exec_after_repeated,
        "exec count should not change for repeated queries on warm cache"
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}
