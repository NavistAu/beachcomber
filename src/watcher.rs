use notify::{Config, Event, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, warn};

pub struct FsWatcher {
    watcher: Box<dyn Watcher + Send>,
    /// Count of underlying kernel `watch()` registrations made by this
    /// instance. Diagnostic/test seam for the scheduler's path-level
    /// registration dedup (canon `provider_source.md` — the kernel watch
    /// call need only happen once per path, not once per subscribing
    /// Source).
    watch_calls: AtomicUsize,
}

impl FsWatcher {
    pub fn new() -> notify::Result<(Self, mpsc::Receiver<Vec<std::path::PathBuf>>)> {
        let (tx, rx) = mpsc::channel(256);
        let watcher = RecommendedWatcher::new(event_handler(tx), Config::default())?;
        Ok((
            Self {
                watcher: Box::new(watcher),
                watch_calls: AtomicUsize::new(0),
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
                watch_calls: AtomicUsize::new(0),
            },
            rx,
        ))
    }

    pub fn watch(&mut self, path: &Path) -> notify::Result<()> {
        debug!("Watching: {:?}", path);
        self.watch_calls.fetch_add(1, Ordering::Relaxed);
        self.watcher.watch(path, RecursiveMode::Recursive)
    }

    pub fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
        debug!("Unwatching: {:?}", path);
        self.watcher.unwatch(path)
    }

    /// Number of underlying kernel `watch()` registrations made so far.
    /// Diagnostic/test seam — see the field doc on `watch_calls`.
    pub fn watch_call_count(&self) -> usize {
        self.watch_calls.load(Ordering::Relaxed)
    }
}

/// Which backend serves provider file-watching, decided once at daemon startup
/// by the watch self-test (canon `provider_source.md` §"Watch backend health").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchBackend {
    /// Kernel-native (FSEvents / inotify) — the self-test confirmed delivery.
    Native,
    /// Polling fallback — the self-test saw no events (stream delivers nothing).
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

/// Watch self-test timeout (canon: 2s). Healthy-idle delivery is ~10ms, but
/// hundreds of ms under heavy filesystem load — the timeout must not
/// misclassify a loaded-but-healthy backend as dead. Concurrent with the
/// scheduler loop, so it costs no startup latency.
pub const WATCH_SELF_TEST_TIMEOUT: Duration = Duration::from_secs(2);

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
                watch_calls: AtomicUsize::new(0),
            },
            rx,
        ))
    }
}

/// Canon §"Watch self-test": register a kernel-native watch on a private temp
/// directory, touch a file inside it, and wait for the event. `false` means
/// the backend creates streams but delivers nothing (seen on sandboxed CI
/// hosts and under a degraded `fseventsd`) — the caller must fall back to
/// polling. Probes the capability, not the environment.
pub async fn self_test_native_backend(timeout: Duration) -> bool {
    // Probe dir via std, not tempfile (a dev-dependency).
    let base = std::env::temp_dir();
    self_test_native_backend_at(&base, timeout).await.is_some()
}

/// Measurement-grade variant: probe event delivery for a watch under `base`,
/// returning the delivery time, or None on timeout/setup failure.
pub async fn self_test_native_backend_at(base: &Path, timeout: Duration) -> Option<Duration> {
    let dir = base.join(format!(".comb-watch-selftest-{}", std::process::id()));
    if std::fs::create_dir_all(&dir).is_err() {
        return None;
    }
    let delivered = async {
        let (mut watcher, mut rx) = FsWatcher::new().ok()?;
        watcher.watch(&dir).ok()?;
        std::fs::write(dir.join("probe"), b"x").ok()?;
        let started = std::time::Instant::now();
        tokio::time::timeout(timeout, rx.recv())
            .await
            .ok()
            .flatten()
            .map(|_| started.elapsed())
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
