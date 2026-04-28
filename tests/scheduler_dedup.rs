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
use std::sync::atomic::{AtomicU32, Ordering};

fn counter_source_meta() -> SourceMetadata {
    SourceMetadata {
        name: "main".into(),
        fields: vec![FieldSchema {
            name: "count".into(),
            field_type: FieldType::Int,
        }],
        scope: SourceScope::Global,
        invalidation: InvalidationStrategy::Poll { interval_secs: 60 },
        keep_alive: KeepAlive::Polls(2),
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 30,
        },
        fsevents_reinstate: false,
    }
}

struct CountingSourceImpl {
    counter: Arc<AtomicU32>,
}

impl Source for CountingSourceImpl {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(counter_source_meta)
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        let count = self.counter.fetch_add(1, Ordering::SeqCst);
        // Simulate some work
        std::thread::sleep(std::time::Duration::from_millis(200));
        let mut result = SourceResult::new();
        result.insert("count", Value::Int(count as i64));
        result
    }
}

struct CountingProvider {
    counter: Arc<AtomicU32>,
}

impl CountingProvider {
    fn new(counter: Arc<AtomicU32>) -> Self {
        Self { counter }
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
        })]
    }
}

#[tokio::test]
async fn rapid_refreshes_are_deduplicated() {
    let exec_count = Arc::new(AtomicU32::new(0));

    let cache = Arc::new(Cache::new());
    let mut registry = ProviderRegistry::new();
    registry
        .register(Box::new(CountingProvider::new(Arc::clone(&exec_count))))
        .expect("counting");
    let registry = Arc::new(registry);
    let config = Config::default();

    let (handle, scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
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

    // Wait for execution to complete.
    // TODO: needs scheduler clock injection — this sleep lets spawn_blocking workers
    // (each sleeping 200 ms real time) complete. Mock clock cannot substitute for
    // real OS-scheduled blocking-pool work, so this remains a real sleep.
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    let count = exec_count.load(Ordering::SeqCst);
    // Should execute significantly fewer than 10 times due to dedup
    // At minimum 1, at most ~2-3 (one running + one queued rerun)
    assert!(
        count < 5,
        "Expected deduplication to reduce 10 refreshes to fewer executions, got {count}"
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}
