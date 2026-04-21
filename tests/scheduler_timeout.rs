use beachcomber::cache::Cache;
use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{
    FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata, ProviderResult, Value,
};
use beachcomber::scheduler::{Scheduler, SchedulerMessage};
use std::sync::Arc;

struct SlowProvider;

impl Provider for SlowProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "slow".to_string(),
            fields: vec![FieldSchema {
                name: "value".to_string(),
                field_type: FieldType::String,
            }],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 60,
                floor_secs: 1,
            },
            global: true,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Option<ProviderResult> {
        // 2s is long enough to exceed the 1s scheduler timeout in this test but
        // short enough that tokio's blocking-pool shutdown (which waits on in-flight
        // spawn_blocking tasks) doesn't block the test-harness exit for 30s.
        std::thread::sleep(std::time::Duration::from_secs(2));
        let mut result = ProviderResult::new();
        result.insert("value", Value::String("done".to_string()));
        Some(result)
    }
}

#[tokio::test]
async fn slow_provider_times_out() {
    let cache = Arc::new(Cache::new());
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(SlowProvider));
    let registry = Arc::new(registry);

    let mut config = Config::default();
    config.daemon.provider_timeout_secs = Some(1);

    let (handle, scheduler) = Scheduler::new(cache.clone(), registry, config);
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    handle
        .send(SchedulerMessage::Refresh {
            provider: "slow".to_string(),
            path: None,
        })
        .await;

    // Wait longer than timeout but shorter than provider sleep
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    // Cache should NOT have the value (timed out)
    assert!(
        cache.get("slow", None).is_none(),
        "Timed-out provider should not populate cache"
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}

#[tokio::test]
async fn fast_provider_completes_within_timeout() {
    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());

    let mut config = Config::default();
    config.daemon.provider_timeout_secs = Some(5);

    let (handle, scheduler) = Scheduler::new(cache.clone(), registry, config);
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    handle
        .send(SchedulerMessage::Refresh {
            provider: "hostname".to_string(),
            path: None,
        })
        .await;

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    assert!(
        cache.get("hostname", None).is_some(),
        "Fast provider should complete within timeout"
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}
