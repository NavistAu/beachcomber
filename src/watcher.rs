use notify::{Config, Event, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, warn};

pub struct FsWatcher {
    watcher: Box<dyn Watcher + Send>,
}

impl FsWatcher {
    pub fn new() -> notify::Result<(Self, mpsc::Receiver<Vec<std::path::PathBuf>>)> {
        let (tx, rx) = mpsc::channel(256);
        let watcher = RecommendedWatcher::new(event_handler(tx), Config::default())?;
        Ok((
            Self {
                watcher: Box::new(watcher),
            },
            rx,
        ))
    }

    /// Polling-backed watcher. Useful on systems where FSEvents / inotify is not
    /// available (sandboxed CI, restricted containers, etc.) and in integration
    /// tests that need deterministic event delivery without requiring kernel
    /// filesystem-notification permissions.
    pub fn new_polling(
        interval: Duration,
    ) -> notify::Result<(Self, mpsc::Receiver<Vec<std::path::PathBuf>>)> {
        let (tx, rx) = mpsc::channel(256);
        // compare_contents(true) detects rewrites whose mtime resolution (second-
        // precision on some macOS volumes) would otherwise hide an in-the-same-second
        // modification. The cost is a per-scan hash, which is negligible for the
        // small trees these tests exercise.
        let config = Config::default()
            .with_poll_interval(interval)
            .with_compare_contents(true);
        let watcher = PollWatcher::new(event_handler(tx), config)?;
        Ok((
            Self {
                watcher: Box::new(watcher),
            },
            rx,
        ))
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

fn event_handler(
    tx: mpsc::Sender<Vec<std::path::PathBuf>>,
) -> impl Fn(Result<Event, notify::Error>) + Send + 'static {
    move |result| match result {
        Ok(event) => {
            if !event.paths.is_empty() {
                let _ = tx.blocking_send(event.paths);
            }
        }
        Err(e) => {
            warn!("Filesystem watch error: {}", e);
        }
    }
}
