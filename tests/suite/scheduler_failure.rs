use beachcomber::cache::Cache;
use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    Source, SourceMetadata, SourceResult, SourceScope,
};
use beachcomber::query::SourceDemand;
use beachcomber::scheduler::{Scheduler, SchedulerMessage};
use beachcomber::watcher_registry::WatcherRegistry;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use crate::common::poll::poll_until_async;

fn failing_source_meta() -> SourceMetadata {
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
    }
}

struct FailingSourceImpl {
    counter: Arc<AtomicU32>,
}

impl Source for FailingSourceImpl {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(failing_source_meta)
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        self.counter.fetch_add(1, Ordering::SeqCst);
        SourceResult::new() // Always returns empty (failure)
    }
}

struct FailingProvider {
    counter: Arc<AtomicU32>,
}

impl FailingProvider {
    fn new(counter: Arc<AtomicU32>) -> Self {
        Self { counter }
    }
}

impl Provider for FailingProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "failing".to_string(),
            sources: vec![failing_source_meta()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(FailingSourceImpl {
            counter: Arc::clone(&self.counter),
        })]
    }
}

#[tokio::test]
async fn repeated_failures_trigger_backoff() {
    let fail_exec_count = Arc::new(AtomicU32::new(0));

    let cache = Arc::new(Cache::new());
    let mut registry = ProviderRegistry::new();
    registry
        .register(Box::new(FailingProvider::new(Arc::clone(&fail_exec_count))))
        .expect("failing");
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

    // Refresh 10 times, each one let to run to completion before the next.
    // That serialisation is what the old inter-refresh sleep bought, and it is
    // what makes this a backoff test rather than a dedup test: a refresh that
    // lands while the previous one is still in flight is deduplicated instead
    // of suppressed. `in_flight` empties only after the failure counter has
    // been recorded, so it is the exact fence.
    //
    // The first status round-trip after each send is also the ordering fence:
    // the channel is FIFO, so it cannot observe an empty `in_flight` from
    // before the refresh was processed.
    for _ in 0..10 {
        handle
            .send(SchedulerMessage::Refresh {
                provider: "failing".to_string(),
                path: None,
                demand: SourceDemand::All,
            })
            .await;
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
    }

    let count = fail_exec_count.load(Ordering::SeqCst);
    // After 3 consecutive failures, subsequent refreshes should be suppressed
    // So we expect fewer than 10 executions
    assert!(
        count < 10,
        "Expected failure backoff to suppress some executions, got {count}"
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}
