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

/// Which backend serves provider file-watching, decided once at daemon startup
/// by the watch self-test (canon `singleton.md` §"Watch self-test").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchBackend {
    /// Kernel-native (FSEvents / inotify) — the self-test confirmed delivery.
    Native,
    /// Polling fallback — the self-test saw no events (e.g. sandboxed daemon).
    Polling,
}

impl WatchBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            WatchBackend::Native => "native",
            WatchBackend::Polling => "polling",
        }
    }
}

/// Watch self-test timeout (canon: 500ms).
pub const WATCH_SELF_TEST_TIMEOUT: Duration = Duration::from_millis(500);

/// Scan interval for the production polling fallback.
pub const POLLING_FALLBACK_INTERVAL: Duration = Duration::from_secs(1);

impl FsWatcher {
    /// Production polling fallback: mtime scan every `interval`, no content
    /// hashing (unlike [`FsWatcher::new_polling`], which is tuned for small
    /// test trees — hashing every file of a watched project root each scan
    /// would not be).
    pub fn new_polling_fallback(
        interval: Duration,
    ) -> notify::Result<(Self, mpsc::Receiver<Vec<std::path::PathBuf>>)> {
        let (tx, rx) = mpsc::channel(256);
        let config = Config::default().with_poll_interval(interval);
        let watcher = PollWatcher::new(event_handler(tx), config)?;
        Ok((
            Self {
                watcher: Box::new(watcher),
            },
            rx,
        ))
    }
}

/// Canon §"Watch self-test": register a kernel-native watch on a private temp
/// directory, touch a file inside it, and wait for the event. `false` means
/// the backend creates streams but delivers nothing — the sandboxed-daemon
/// failure mode — and the caller must fall back to polling. Probes the
/// capability, not the environment.
pub async fn self_test_native_backend(timeout: Duration) -> bool {
    // Probe dir via std, not tempfile (a dev-dependency). Any writable
    // location works — this is not socket-path resolution.
    let dir = std::env::temp_dir().join(format!(".comb-watch-selftest-{}", std::process::id()));
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let delivered = async {
        let Ok((mut watcher, mut rx)) = FsWatcher::new() else {
            return false;
        };
        if watcher.watch(&dir).is_err() {
            return false;
        }
        if std::fs::write(dir.join("probe"), b"x").is_err() {
            return false;
        }
        matches!(tokio::time::timeout(timeout, rx.recv()).await, Ok(Some(_)))
    }
    .await;
    let _ = std::fs::remove_dir_all(&dir);
    delivered
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
