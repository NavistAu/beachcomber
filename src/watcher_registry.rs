use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::broadcast;

type WatchKey = String;

fn make_watch_key(provider: &str, path: Option<&str>) -> WatchKey {
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

    /// Internal accessor exposed for integration tests in `tests/watch_registry.rs`
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
