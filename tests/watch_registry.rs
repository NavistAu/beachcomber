use beachcomber::watcher_registry::WatcherRegistry;
use std::sync::Arc;

#[tokio::test]
async fn subscribe_and_receive_notification() {
    let registry = Arc::new(WatcherRegistry::new());
    let mut rx = registry.subscribe("git", None);
    registry.notify("git", None);
    let result = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
    assert!(result.is_ok(), "Should receive notification");
}

#[tokio::test]
async fn no_notification_for_different_key() {
    let registry = Arc::new(WatcherRegistry::new());
    let mut rx = registry.subscribe("git", None);
    registry.notify("battery", None);
    let result = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await;
    assert!(
        result.is_err(),
        "Should not receive notification for different key"
    );
}

#[tokio::test]
async fn path_scoped_notifications() {
    let registry = Arc::new(WatcherRegistry::new());
    let mut rx_a = registry.subscribe("git", Some("/proj-a"));
    let mut rx_b = registry.subscribe("git", Some("/proj-b"));
    registry.notify("git", Some("/proj-a"));
    let result_a = tokio::time::timeout(std::time::Duration::from_millis(100), rx_a.recv()).await;
    assert!(result_a.is_ok());
    let result_b = tokio::time::timeout(std::time::Duration::from_millis(50), rx_b.recv()).await;
    assert!(result_b.is_err());
}

#[tokio::test]
async fn multiple_subscribers_all_receive() {
    let registry = Arc::new(WatcherRegistry::new());
    let mut rx1 = registry.subscribe("git", None);
    let mut rx2 = registry.subscribe("git", None);
    registry.notify("git", None);
    let r1 = tokio::time::timeout(std::time::Duration::from_millis(100), rx1.recv()).await;
    let r2 = tokio::time::timeout(std::time::Duration::from_millis(100), rx2.recv()).await;
    assert!(r1.is_ok());
    assert!(r2.is_ok());
}

#[tokio::test]
async fn entry_removed_on_notify_after_last_receiver_dropped() {
    let registry = Arc::new(WatcherRegistry::new());
    {
        let _rx = registry.subscribe("git", Some("/proj"));
    }
    // With the lifecycle hook, the entry was already reclaimed on drop.
    // notify() on a missing key is a no-op.
    registry.notify("git", Some("/proj"));
    assert_eq!(
        registry.entry_count(),
        0,
        "Lifecycle hook should have removed the entry on drop"
    );
    // Fresh subscription after reclaim should work normally.
    let mut rx = registry.subscribe("git", Some("/proj"));
    registry.notify("git", Some("/proj"));
    let result = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
    assert!(
        result.is_ok(),
        "fresh subscription should still deliver after GC"
    );
}

#[tokio::test]
async fn notify_does_not_remove_entry_with_live_receiver() {
    let registry = Arc::new(WatcherRegistry::new());
    let mut rx = registry.subscribe("git", None);
    registry.notify("git", None);
    let first = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
    assert!(first.is_ok());
    registry.notify("git", None);
    let second = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
    assert!(second.is_ok());
}
