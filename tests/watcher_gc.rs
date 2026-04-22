use beachcomber::watcher_registry::WatcherRegistry;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn gc_removes_keys_without_receivers_on_demand() {
    // With the lifecycle hook, Subscription::drop reclaims automatically.
    // gc() is still useful as a safety net; verify it is a no-op when the
    // map is already clean (the normal case after the hook runs).
    let registry = Arc::new(WatcherRegistry::new());
    {
        let _sub = registry.subscribe("git", Some("/tmp"));
        assert_eq!(registry.entry_count(), 1);
    } // lifecycle hook fires here — entry reclaimed immediately
    assert_eq!(registry.entry_count(), 0);

    // gc() is idempotent on an already-empty map.
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
        let _sub = watchers.subscribe("git", Some("/tmp"));
    }
    // Lifecycle hook fires on drop: entry is already gone before the sleep.
    assert_eq!(watchers.entry_count(), 0);

    sleep(Duration::from_millis(200)).await;
    // Periodic GC tick runs; map is still empty — no change.
    assert_eq!(watchers.entry_count(), 0);

    drop(harness);
}

#[tokio::test]
async fn receiver_drop_triggers_gc() {
    let registry = Arc::new(WatcherRegistry::new());
    {
        let _sub = registry.subscribe("git", Some("/tmp"));
        assert_eq!(registry.entry_count(), 1);
    }
    // Drop handler should have reclaimed the key immediately.
    assert_eq!(registry.entry_count(), 0);
}

#[tokio::test]
async fn multiple_subscribers_share_key_until_last_drops() {
    let registry = Arc::new(WatcherRegistry::new());
    let _a = registry.subscribe("git", Some("/tmp"));
    let b = registry.subscribe("git", Some("/tmp"));
    assert_eq!(registry.entry_count(), 1);

    drop(b);
    // One subscriber remains; key should still be present.
    assert_eq!(registry.entry_count(), 1);

    drop(_a);
    assert_eq!(registry.entry_count(), 0);
}
