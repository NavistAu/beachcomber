use dashmap::DashMap;
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

impl WatcherRegistry {
    pub fn new() -> Self {
        Self {
            channels: DashMap::new(),
        }
    }

    pub fn subscribe(&self, provider: &str, path: Option<&str>) -> broadcast::Receiver<()> {
        let key = make_watch_key(provider, path);
        let entry = self.channels.entry(key).or_insert_with(|| {
            let (tx, _) = broadcast::channel(64);
            tx
        });
        entry.value().subscribe()
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
