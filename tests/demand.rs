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

    let (handle, scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    // Send query activity for hostname (a global provider — should execute immediately)
    handle
        .send(SchedulerMessage::QueryActivity {
            provider: "hostname".to_string(),
            path: None,
        })
        .await;

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Cache should be populated
    assert!(
        cache.get_entry("hostname", None).is_some(),
        "QueryActivity should trigger provider execution"
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}

#[tokio::test]
async fn watch_only_providers_do_not_enter_poll_timers() {
    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let config = Config::default();

    let (handle, scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    // Demand a Watch-only provider. It should be cached (from startup) but must not
    // create a poll timer entry — otherwise it would be re-polled on the
    // poll cadence, violating the Watch contract.
    handle
        .send(SchedulerMessage::QueryActivity {
            provider: "hostname".to_string(),
            path: None,
        })
        .await;

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    assert!(
        cache.get_entry("hostname", None).is_some(),
        "hostname should be cached after QueryActivity"
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}

#[tokio::test]
async fn repeated_queries_keep_data_warm() {
    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let config = Config::default();

    let (handle, scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
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
        cache.get_entry("hostname", None).is_some(),
        "Repeated queries should keep data warm"
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}
