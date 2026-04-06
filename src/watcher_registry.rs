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
        if let Some(tx) = self.channels.get(&key) {
            let _ = tx.send(());
        }
    }

    pub fn gc(&self) {
        self.channels.retain(|_, tx| tx.receiver_count() > 0);
    }
}

impl Default for WatcherRegistry {
    fn default() -> Self {
        Self::new()
    }
}
