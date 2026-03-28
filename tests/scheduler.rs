use beachcomber::cache::Cache;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::scheduler::{Scheduler, SchedulerMessage, TriggerSet};
use std::sync::Arc;

#[tokio::test]
async fn scheduler_poke_populates_cache() {
    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let config = beachcomber::config::Config::default();

    let (handle, scheduler) = Scheduler::new(cache.clone(), registry, config);
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    handle.send(SchedulerMessage::Poke {
        provider: "hostname".to_string(),
        path: None,
    }).await;

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let entry = cache.get("hostname", None);
    assert!(entry.is_some(), "Poke should populate cache");
    assert!(!entry.unwrap().result.get("name").unwrap().as_text().is_empty());

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}

#[tokio::test]
async fn scheduler_poke_unknown_provider() {
    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let config = beachcomber::config::Config::default();

    let (handle, scheduler) = Scheduler::new(cache.clone(), registry, config);
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    handle.send(SchedulerMessage::Poke {
        provider: "nonexistent".to_string(),
        path: None,
    }).await;

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}

#[tokio::test]
async fn scheduler_subscribe_starts_polling() {
    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let config = beachcomber::config::Config::default();

    let (handle, scheduler) = Scheduler::new(cache.clone(), registry, config);
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    handle.send(SchedulerMessage::Subscribe {
        consumer_id: 1,
        provider: "hostname".to_string(),
        path: None,
        triggers: TriggerSet { watch: false, poll_secs: Some(1) },
    }).await;

    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    let entry = cache.get("hostname", None);
    assert!(entry.is_some(), "Polling should populate cache");

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}

#[tokio::test]
async fn scheduler_shutdown() {
    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let config = beachcomber::config::Config::default();

    let (handle, scheduler) = Scheduler::new(cache, registry, config);
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    handle.send(SchedulerMessage::Shutdown).await;

    let result = tokio::time::timeout(std::time::Duration::from_secs(2), sched_task).await;
    assert!(result.is_ok(), "Scheduler should shut down within timeout");
}
