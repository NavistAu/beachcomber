//! End-to-end tests for the cache lifecycle.
//! Unit tests for the registry itself live in src/scheduler/lifecycle.rs.
//! These exercise the full scheduler + cache + a fake provider.

use beachcomber::cache::Cache;
use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{
    FieldSchema, FieldScope, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};
use beachcomber::scheduler::{Scheduler, SchedulerMessage};
use beachcomber::watcher_registry::WatcherRegistry;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

static POLL_COUNT: AtomicU32 = AtomicU32::new(0);

/// A simple global provider that increments a counter on each execute.
struct CountingGlobalProvider;

impl Provider for CountingGlobalProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "lc_counter".to_string(),
            fields: vec![FieldSchema {
                name: "value".to_string(),
                field_type: FieldType::Int,
                scope: FieldScope::Global,
            }],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 5,
                floor_secs: 1,
            },
        }
    }

    fn execute(&self, _path: Option<&str>) -> Vec<(Option<String>, ProviderResult)> {
        let count = POLL_COUNT.fetch_add(1, Ordering::SeqCst);
        let mut result = ProviderResult::new();
        result.insert("value", Value::Int(count as i64));
        vec![(None, result)]
    }
}

/// Verifies that a cold cache miss via QueryActivity immediately triggers provider execution
/// and the cache entry is populated.
#[tokio::test]
async fn integration_cold_miss_populates_cache_and_enters_active() {
    POLL_COUNT.store(0, Ordering::SeqCst);

    let cache = Arc::new(Cache::new());
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(CountingGlobalProvider));
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
        cache.get("lc_counter", None).is_none(),
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
        cache.get("lc_counter", None).is_some(),
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
    registry.register(Box::new(CountingGlobalProvider));
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

    // Active entries appear in demand, not backoff.
    assert!(
        status.demand.iter().any(|d| d.provider == "lc_counter"),
        "lc_counter should appear in demand when Active; got {:?}",
        status.demand
    );
    assert!(
        !status.backoff.iter().any(|b| b.provider == "lc_counter"),
        "lc_counter should NOT be in backoff when Active; got {:?}",
        status.backoff
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
    registry.register(Box::new(CountingGlobalProvider));
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

    let gen_after_first = cache
        .get("lc_counter", None)
        .expect("should have entry")
        .generation;
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
    let gen_after_repeated = cache
        .get("lc_counter", None)
        .expect("should still have entry")
        .generation;

    // Generation should be the same (no re-execution triggered by repeat queries).
    assert_eq!(
        gen_after_first, gen_after_repeated,
        "repeated queries on warm cache should not re-execute provider"
    );
    assert_eq!(
        exec_after_first, exec_after_repeated,
        "exec count should not change for repeated queries on warm cache"
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}
