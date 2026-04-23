use beachcomber::cache::Cache;
use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{
    FieldSchema, FieldScope, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult,
};
use beachcomber::scheduler::{Scheduler, SchedulerMessage};
use beachcomber::watcher_registry::WatcherRegistry;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

static FAIL_EXEC_COUNT: AtomicU32 = AtomicU32::new(0);

struct FailingProvider;

impl Provider for FailingProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "failing".to_string(),
            fields: vec![FieldSchema {
                name: "value".to_string(),
                field_type: FieldType::String,
                scope: FieldScope::Global,
            }],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 60,
                floor_secs: 1,
            },
            global: true,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Option<ProviderResult> {
        FAIL_EXEC_COUNT.fetch_add(1, Ordering::SeqCst);
        None // Always fails
    }
}

#[tokio::test]
async fn repeated_failures_trigger_backoff() {
    FAIL_EXEC_COUNT.store(0, Ordering::SeqCst);

    let cache = Arc::new(Cache::new());
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(FailingProvider));
    let registry = Arc::new(registry);
    let config = Config::default();

    let (handle, scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    // Refresh 10 times with small delays
    for _ in 0..10 {
        handle
            .send(SchedulerMessage::Refresh {
                provider: "failing".to_string(),
                path: None,
            })
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let count = FAIL_EXEC_COUNT.load(Ordering::SeqCst);
    // After 3 consecutive failures, subsequent refreshes should be suppressed
    // So we expect fewer than 10 executions
    assert!(
        count < 10,
        "Expected failure backoff to suppress some executions, got {count}"
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}
