use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use tokio::sync::mpsc;
use tracing::{debug, warn};

pub struct FsWatcher {
    watcher: RecommendedWatcher,
}

impl FsWatcher {
    pub fn new() -> notify::Result<(Self, mpsc::Receiver<Vec<std::path::PathBuf>>)> {
        let (tx, rx) = mpsc::channel(256);

        let watcher = RecommendedWatcher::new(
            move |result: Result<Event, notify::Error>| match result {
                Ok(event) => {
                    if !event.paths.is_empty() {
                        let _ = tx.blocking_send(event.paths);
                    }
                }
                Err(e) => {
                    warn!("Filesystem watch error: {}", e);
                }
            },
            Config::default(),
        )?;

        Ok((Self { watcher }, rx))
    }

    pub fn watch(&mut self, path: &Path) -> notify::Result<()> {
        debug!("Watching: {:?}", path);
        self.watcher.watch(path, RecursiveMode::Recursive)
    }

    pub fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
        debug!("Unwatching: {:?}", path);
        self.watcher.unwatch(path)
    }
}
