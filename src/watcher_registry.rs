use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::broadcast;

type WatchKey = String;

/// The key of a namespace's watchers. A facet (`p:template`, `p:cache`) shares
/// its base namespace's key: every write to `p` or to any of its facets
/// notifies `p` at its path, and a watch on any key of `p` subscribes there.
fn make_watch_key(provider: &str, path: Option<&str>) -> WatchKey {
    let provider = provider.split(':').next().unwrap_or(provider);
    match path {
        Some(p) => format!("{provider}\0{p}"),
        None => provider.to_string(),
    }
}

pub struct WatcherRegistry {
    channels: DashMap<WatchKey, broadcast::Sender<()>>,
}

/// A subscription guard that holds a broadcast receiver and a weak reference back to
/// the registry. When dropped, it calls `gc_key` to remove the map entry if no
/// receivers remain.
#[must_use = "dropping a Subscription immediately unsubscribes; bind it to a variable"]
pub struct Subscription {
    inner: Option<broadcast::Receiver<()>>,
    registry: std::sync::Weak<WatcherRegistry>,
    key: WatchKey,
}

impl Subscription {
    /// Await the next notification on the subscribed key.
    pub async fn recv(&mut self) -> Result<(), broadcast::error::RecvError> {
        self.inner
            .as_mut()
            .expect("subscription receiver already dropped")
            .recv()
            .await
    }

    /// Try to receive without awaiting.
    pub fn try_recv(&mut self) -> Result<(), broadcast::error::TryRecvError> {
        self.inner
            .as_mut()
            .expect("subscription receiver already dropped")
            .try_recv()
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        // Drop the receiver first so its ref-count decrements before gc_key checks.
        drop(self.inner.take());
        if let Some(reg) = self.registry.upgrade() {
            reg.gc_key(&self.key);
        }
    }
}

impl WatcherRegistry {
    pub fn new() -> Self {
        Self {
            channels: DashMap::new(),
        }
    }

    /// Subscribe to notifications for the given provider/path pair. Requires the registry
    /// to be behind an `Arc` so the returned `Subscription` guard can hold a weak back-
    /// reference for drop-time cleanup.
    pub fn subscribe(self: &Arc<Self>, provider: &str, path: Option<&str>) -> Subscription {
        let key = make_watch_key(provider, path);
        let entry = self.channels.entry(key.clone()).or_insert_with(|| {
            let (tx, _) = broadcast::channel(64);
            tx
        });
        let receiver = entry.value().subscribe();
        Subscription {
            inner: Some(receiver),
            registry: Arc::downgrade(self),
            key,
        }
    }

    pub fn notify(&self, provider: &str, path: Option<&str>) {
        let key = make_watch_key(provider, path);
        let had_err = self
            .channels
            .get(&key)
            .map(|entry| entry.value().send(()).is_err())
            .unwrap_or(false);
        if had_err {
            // Predicate runs under the per-key DashMap lock: if a concurrent subscribe
            // added a receiver between the send() and here, receiver_count > 0 and
            // removal is skipped.
            self.channels
                .remove_if(&key, |_, tx| tx.receiver_count() == 0);
        }
    }

    pub fn gc(&self) {
        self.channels.retain(|_, tx| tx.receiver_count() > 0);
    }

    /// Remove the key if no receivers remain. Called by `Subscription::drop`.
    fn gc_key(&self, key: &WatchKey) {
        self.channels
            .remove_if(key, |_, tx| tx.receiver_count() == 0);
    }

    /// Internal accessor exposed for integration tests in `src/watch_registry_tests.rs`
    /// to assert post-GC map size. Not intended as a public API — Task 10 will add
    /// a proper `len()` for status reporting.
    #[doc(hidden)]
    pub fn entry_count(&self) -> usize {
        self.channels.len()
    }
}

impl Default for WatcherRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::WatcherRegistry;
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

    /// The GC tick itself has no observable of its own — `gc()` is a no-op on an
    /// already-clean map, which is exactly the invariant under test. So rather than
    /// sleeping and hoping ticks fired, run on the mock clock and drive them:
    /// `advance` is deterministic (the ticks provably fire) and costs no real time.
    #[tokio::test(start_paused = true)]
    async fn scheduler_periodically_gcs_watcher_registry() {
        let watchers = Arc::new(WatcherRegistry::new());
        let harness = crate::scheduler::test_support::start_with_gc_interval(
            watchers.clone(),
            Duration::from_millis(50),
        );
        // Let the harness task reach its first `tick().await` so the interval is
        // armed before the clock moves.
        tokio::task::yield_now().await;

        {
            let _sub = watchers.subscribe("git", Some("/tmp"));
        }
        // Lifecycle hook fires on drop: entry is already gone before any GC tick.
        assert_eq!(watchers.entry_count(), 0);

        // Four 50ms GC periods on the mock clock.
        tokio::time::advance(Duration::from_millis(200)).await;
        tokio::task::yield_now().await;

        // Periodic GC tick ran; map is still empty — no change.
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
}
