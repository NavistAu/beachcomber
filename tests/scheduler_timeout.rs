use beachcomber::cache::Cache;
use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use beachcomber::scheduler::{Scheduler, SchedulerMessage};
use beachcomber::watcher_registry::WatcherRegistry;
use std::sync::Arc;

fn slow_source_meta() -> SourceMetadata {
    SourceMetadata {
        name: "main".into(),
        fields: vec![FieldSchema {
            name: "value".into(),
            field_type: FieldType::String,
        }],
        scope: SourceScope::Global,
        invalidation: InvalidationStrategy::Poll { interval_secs: 60 },
        keep_alive: KeepAlive::Polls(2),
        failback: FailbackConfig { reattempts: 3, interval_secs: 30 },
        fsevents_reinstate: false,
    }
}

struct SlowSourceImpl;

impl Source for SlowSourceImpl {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(slow_source_meta)
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        // 2s is long enough to exceed the 1s scheduler timeout in this test but
        // short enough that tokio's blocking-pool shutdown (which waits on in-flight
        // spawn_blocking tasks) doesn't block the test-harness exit for 30s.
        std::thread::sleep(std::time::Duration::from_secs(2));
        let mut result = SourceResult::new();
        result.insert("value", Value::String("done".to_string()));
        result
    }
}

struct SlowProvider;

impl Provider for SlowProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "slow".to_string(),
            sources: vec![slow_source_meta()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(SlowSourceImpl)]
    }
}

#[tokio::test]
async fn slow_provider_times_out() {
    let cache = Arc::new(Cache::new());
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(SlowProvider)).expect("slow");
    let registry = Arc::new(registry);

    let mut config = Config::default();
    config.daemon.provider_timeout_secs = Some(1);

    let (handle, scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
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
        cache.get_entry("slow", None).is_none(),
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

    let (handle, scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    handle
        .send(SchedulerMessage::Refresh {
            provider: "hostname".to_string(),
            path: None,
        })
        .await;

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    assert!(
        cache.get_entry("hostname", None).is_some(),
        "Fast provider should complete within timeout"
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}
