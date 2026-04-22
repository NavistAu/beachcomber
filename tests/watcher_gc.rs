use std::sync::Arc;
use std::time::Duration;
use beachcomber::watcher_registry::WatcherRegistry;

#[tokio::test]
async fn gc_removes_keys_with_no_receivers() {
    let registry = Arc::new(WatcherRegistry::new());
    {
        let _rx = registry.subscribe("git", Some("/tmp"));
        assert_eq!(registry.entry_count(), 1);
    } // _rx dropped here

    // Without the lifecycle hook (T09), the entry lingers until gc() runs.
    assert_eq!(registry.entry_count(), 1);
    registry.gc();
    assert_eq!(registry.entry_count(), 0);
}

#[tokio::test]
async fn scheduler_periodically_gcs_watcher_registry() {
    use tokio::time::sleep;

    let watchers = Arc::new(WatcherRegistry::new());
    let harness = beachcomber::scheduler::test_support::start_with_gc_interval(
        watchers.clone(),
        Duration::from_millis(50),
    );

    {
        let _rx = watchers.subscribe("git", Some("/tmp"));
    }
    assert_eq!(watchers.entry_count(), 1);

    sleep(Duration::from_millis(200)).await;
    assert_eq!(watchers.entry_count(), 0);

    drop(harness);
}
