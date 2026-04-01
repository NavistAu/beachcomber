use beachcomber::cache::Cache;
use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::scheduler::Scheduler;
use std::sync::Arc;

#[tokio::test]
async fn scheduler_shuts_down_when_idle() {
    let cache = Arc::new(Cache::new());
    // Empty registry — no Once providers to compute
    let registry = Arc::new(ProviderRegistry::new());

    let mut config = Config::default();
    config.lifecycle.idle_shutdown_secs = Some(1); // Shut down after 1s idle

    let (_handle, scheduler) = Scheduler::new(cache, registry, config);

    // Scheduler should exit on its own after idle timeout
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::spawn(async move { scheduler.run().await }),
    )
    .await;

    assert!(
        result.is_ok(),
        "Scheduler should shut down within idle timeout"
    );
}
