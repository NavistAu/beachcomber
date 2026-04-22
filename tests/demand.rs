use beachcomber::cache::Cache;
use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::scheduler::{Scheduler, SchedulerMessage};
use beachcomber::watcher_registry::WatcherRegistry;
use std::sync::Arc;

#[tokio::test]
async fn query_activity_triggers_provider_execution() {
    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let config = Config::default();

    let (handle, scheduler) = Scheduler::new(cache.clone(), registry, config, Arc::new(WatcherRegistry::new()));
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    // Send query activity for hostname (a Once provider — should execute immediately)
    handle
        .send(SchedulerMessage::QueryActivity {
            provider: "hostname".to_string(),
            path: None,
        })
        .await;

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Cache should be populated
    assert!(
        cache.get("hostname", None).is_some(),
        "QueryActivity should trigger provider execution"
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}

#[tokio::test]
async fn query_activity_sets_up_polling() {
    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let config = Config::default();

    let (handle, scheduler) = Scheduler::new(cache.clone(), registry, config, Arc::new(WatcherRegistry::new()));
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    // Query load provider (Poll with 10s interval)
    handle
        .send(SchedulerMessage::QueryActivity {
            provider: "load".to_string(),
            path: None,
        })
        .await;

    // Wait for initial execution + one poll cycle
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let entry1 = cache.get("load", None);
    assert!(entry1.is_some(), "Should have load data after demand");

    // Wait for a poll to refresh it
    tokio::time::sleep(std::time::Duration::from_secs(11)).await;

    let entry2 = cache.get("load", None);
    assert!(entry2.is_some(), "Should still have load data after poll");
    // The generation should have advanced (data was refreshed)
    assert!(
        entry2.unwrap().generation > entry1.unwrap().generation,
        "Data should have been refreshed by poll"
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}

#[tokio::test]
async fn repeated_queries_keep_data_warm() {
    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let config = Config::default();

    let (handle, scheduler) = Scheduler::new(cache.clone(), registry, config, Arc::new(WatcherRegistry::new()));
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    // Simulate repeated queries (like a statusline)
    for _ in 0..5 {
        handle
            .send(SchedulerMessage::QueryActivity {
                provider: "hostname".to_string(),
                path: None,
            })
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    assert!(
        cache.get("hostname", None).is_some(),
        "Repeated queries should keep data warm"
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}
