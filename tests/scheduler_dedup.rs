use beachcomber::cache::Cache;
use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{
    FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata, ProviderResult, Value,
};
use beachcomber::scheduler::{Scheduler, SchedulerMessage};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

static EXEC_COUNT: AtomicU32 = AtomicU32::new(0);

struct CountingProvider;

impl Provider for CountingProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "counter".to_string(),
            fields: vec![FieldSchema {
                name: "count".to_string(),
                field_type: FieldType::Int,
            }],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 60,
                floor_secs: 1,
            },
            global: true,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Option<ProviderResult> {
        let count = EXEC_COUNT.fetch_add(1, Ordering::SeqCst);
        // Simulate some work
        std::thread::sleep(std::time::Duration::from_millis(200));
        let mut result = ProviderResult::new();
        result.insert("count", Value::Int(count as i64));
        Some(result)
    }
}

#[tokio::test]
async fn rapid_refreshes_are_deduplicated() {
    EXEC_COUNT.store(0, Ordering::SeqCst);

    let cache = Arc::new(Cache::new());
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(CountingProvider));
    let registry = Arc::new(registry);
    let config = Config::default();

    let (handle, scheduler) = Scheduler::new(cache.clone(), registry, config);
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    // Send 10 rapid refreshes
    for _ in 0..10 {
        handle
            .send(SchedulerMessage::Refresh {
                provider: "counter".to_string(),
                path: None,
            })
            .await;
    }

    // Wait for execution to complete
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    let exec_count = EXEC_COUNT.load(Ordering::SeqCst);
    // Should execute significantly fewer than 10 times due to dedup
    // At minimum 1, at most ~2-3 (one running + one queued rerun)
    assert!(
        exec_count < 5,
        "Expected deduplication to reduce 10 refreshes to fewer executions, got {exec_count}"
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}
