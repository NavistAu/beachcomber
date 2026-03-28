use beachcomber::cache::Cache;
use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::scheduler::{Scheduler, SchedulerMessage, TriggerSet};
use std::sync::Arc;

#[tokio::test]
async fn unsubscribe_last_consumer_keeps_cache_during_grace() {
    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let config = Config::default();

    let (handle, scheduler) = Scheduler::new(cache.clone(), registry, config);
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    handle.send(SchedulerMessage::Subscribe {
        consumer_id: 1,
        provider: "hostname".to_string(),
        path: None,
        triggers: TriggerSet { watch: false, poll_secs: Some(1) },
    }).await;

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    handle.send(SchedulerMessage::Unsubscribe {
        consumer_id: 1,
        provider: "hostname".to_string(),
        path: None,
    }).await;

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    assert!(cache.get("hostname", None).is_some(),
            "Cache entry should survive during grace period");

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}
