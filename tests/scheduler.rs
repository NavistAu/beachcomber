use beachcomber::cache::Cache;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::scheduler::{Scheduler, SchedulerMessage};
use beachcomber::watcher_registry::WatcherRegistry;
use std::sync::Arc;
use std::time::Duration;

mod common;
use common::poll::poll_until_async;

#[tokio::test]
async fn scheduler_refresh_populates_cache() {
    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let config = beachcomber::config::Config::default();

    let (handle, mut scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
    scheduler.use_polling_watcher(Duration::from_millis(50));
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    handle
        .send(SchedulerMessage::Refresh {
            provider: "hostname".to_string(),
            path: None,
        })
        .await;

    let entry = poll_until_async(
        "hostname cache entry",
        "Some(_)",
        Duration::from_secs(5),
        Duration::from_millis(10),
        async || cache.get_entry("hostname", None),
        |e| e.is_some(),
    )
    .await;
    assert!(entry.is_some(), "Refresh should populate cache");
    let _entry = entry.unwrap();
    // hostname.host source should have a 'name' field
    let name_value = cache.get_field("hostname", None, "name");
    assert!(
        name_value.is_some(),
        "hostname 'name' field should be populated"
    );
    assert!(!name_value.unwrap().0.as_text().is_empty());

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}

#[tokio::test]
async fn scheduler_refresh_unknown_provider() {
    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let config = beachcomber::config::Config::default();

    let (handle, mut scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
    scheduler.use_polling_watcher(Duration::from_millis(50));
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    handle
        .send(SchedulerMessage::Refresh {
            provider: "nonexistent".to_string(),
            path: None,
        })
        .await;

    // The channel is FIFO: a status round-trip that completes proves the
    // unknown-provider Refresh was already processed (and did not wedge the
    // loop), which is the whole point of the test.
    assert!(
        handle.get_status().await.is_some(),
        "scheduler should still serve status after an unknown-provider refresh"
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}

#[tokio::test]
async fn scheduler_shutdown() {
    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let config = beachcomber::config::Config::default();

    let (handle, mut scheduler) =
        Scheduler::new(cache, registry, config, Arc::new(WatcherRegistry::new()));
    scheduler.use_polling_watcher(Duration::from_millis(50));
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    handle.send(SchedulerMessage::Shutdown).await;

    let result = tokio::time::timeout(Duration::from_secs(2), sched_task).await;
    assert!(result.is_ok(), "Scheduler should shut down within timeout");
}
