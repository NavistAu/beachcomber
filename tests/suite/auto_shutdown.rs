use beachcomber::cache::Cache;
use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::scheduler::Scheduler;
use beachcomber::watcher_registry::WatcherRegistry;
use std::sync::Arc;

/// On paused time: the second of idleness and the tick that notices it
/// elapse as soon as the runtime has nothing else to do.
#[tokio::test(start_paused = true)]
async fn scheduler_shuts_down_when_idle() {
    let cache = Arc::new(Cache::new());
    // Empty registry — no Once providers to compute
    let registry = Arc::new(ProviderRegistry::new());

    // Shut down after 1s idle.
    let config = Config::from_toml("[daemon]\nidle_shutdown = \"1s\"\n");

    let (_handle, scheduler) =
        Scheduler::new(cache, registry, config, Arc::new(WatcherRegistry::new()));

    // Scheduler should exit on its own after idle timeout
    let started = tokio::time::Instant::now();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::spawn(async move { scheduler.run().await }),
    )
    .await;

    assert!(
        result.is_ok(),
        "Scheduler should shut down within idle timeout"
    );
    assert!(
        started.elapsed() >= std::time::Duration::from_secs(1),
        "and not before it: {:?}",
        started.elapsed()
    );
}
