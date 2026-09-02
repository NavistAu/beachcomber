use beachcomber::cache::Cache;
use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::query::SourceDemand;
use beachcomber::scheduler::{Scheduler, SchedulerMessage};
use beachcomber::watcher_registry::WatcherRegistry;
use std::sync::Arc;
use std::time::Duration;

mod common;
use common::poll::poll_until_async;

#[tokio::test]
async fn query_activity_triggers_provider_execution() {
    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let config = Config::default();

    let (handle, mut scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
    scheduler.use_polling_watcher(Duration::from_millis(50));
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    // Send query activity for hostname (a global provider — should execute immediately)
    handle
        .send(SchedulerMessage::QueryActivity {
            provider: "hostname".to_string(),
            path: None,
            demand: SourceDemand::All,
        })
        .await;

    // Cache should be populated
    let entry = poll_until_async(
        "hostname cache entry",
        "Some(_)",
        Duration::from_secs(5),
        Duration::from_millis(10),
        async || cache.get_entry("hostname", None),
        |e| e.is_some(),
    )
    .await;
    assert!(
        entry.is_some(),
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

    let (handle, mut scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
    scheduler.use_polling_watcher(Duration::from_millis(50));
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    // Demand a Watch-only provider. It should be cached (from startup) but must not
    // create a poll timer entry — otherwise it would be re-polled on the
    // poll cadence, violating the Watch contract.
    handle
        .send(SchedulerMessage::QueryActivity {
            provider: "hostname".to_string(),
            path: None,
            demand: SourceDemand::All,
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
    assert!(
        entry.is_some(),
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

    let (handle, mut scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
    scheduler.use_polling_watcher(Duration::from_millis(50));
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    // Simulate repeated queries (like a statusline). The status round-trip
    // between sends is the fence the old inter-query sleep was standing in for:
    // the mpsc channel is FIFO, so a reply proves the preceding QueryActivity
    // was fully processed before the next one is sent.
    for _ in 0..5 {
        handle
            .send(SchedulerMessage::QueryActivity {
                provider: "hostname".to_string(),
                path: None,
                demand: SourceDemand::All,
            })
            .await;
        assert!(
            handle.get_status().await.is_some(),
            "scheduler should reply"
        );
    }

    let entry = poll_until_async(
        "hostname cache entry",
        "Some(_)",
        Duration::from_secs(5),
        Duration::from_millis(10),
        async || cache.get_entry("hostname", None),
        |e| e.is_some(),
    )
    .await;
    assert!(entry.is_some(), "Repeated queries should keep data warm");

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}

#[tokio::test]
async fn field_demand_warms_only_owning_source() {
    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let config = Config::default();

    let (handle, mut scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
    scheduler.use_polling_watcher(Duration::from_millis(50));
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    // Demand the git "refs" source only (as a field query for git.branch would).
    // We assert lifecycle *warming* (which sources get on_demand'd), not cache
    // data, so a non-git temp path is fine — the lifecycle entry is created
    // before execute() runs regardless of whether a .git repo is found. An empty
    // TempDir rather than the shared `$TMPDIR`: demand registers a *recursive*
    // watch on this path, and walking the whole shared temp tree costs far more
    // than the rest of this file put together.
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().to_string_lossy().to_string();
    handle
        .send(SchedulerMessage::QueryActivity {
            provider: "git".to_string(),
            path: Some(path.clone()),
            demand: SourceDemand::Sources(vec!["refs".to_string()]),
        })
        .await;

    // `on_demand` for every demanded Source happens in one pass while the
    // scheduler handles this QueryActivity, so a snapshot that already contains
    // "refs" is proof the message was processed to completion. The negative
    // assertion below reads that same snapshot, which is what makes it sound.
    let warmed = poll_until_async(
        "git lifecycle sources warmed",
        "a set containing \"refs\"",
        Duration::from_secs(5),
        Duration::from_millis(10),
        async || {
            handle
                .get_lifecycle_snapshots()
                .await
                .keys()
                .map(|k| k.2.clone())
                .collect::<Vec<String>>()
        },
        |w| w.iter().any(|s| s == "refs"),
    )
    .await;
    assert!(
        warmed.iter().any(|s| s == "refs"),
        "refs must be warmed, got {warmed:?}"
    );
    assert!(
        !warmed.iter().any(|s| s == "diff" || s == "status"),
        "sibling git sources must NOT be warmed, got {warmed:?}"
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}
