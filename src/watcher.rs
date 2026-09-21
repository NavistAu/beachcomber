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
        // Kernel watch registration blocks — ~1.2s per `watch()` measured for
        // FSEvents on a loaded host — so it runs on the blocking pool rather
        // than on the async worker this task shares with the scheduler loop.
        let probe_dir = dir.clone();
        let (watcher, mut rx, started) = tokio::task::spawn_blocking(move || {
            let (mut watcher, rx) = FsWatcher::new().ok()?;
            watcher.watch(&probe_dir).ok()?;
            std::fs::write(probe_dir.join("probe"), b"x").ok()?;
            Some((watcher, rx, std::time::Instant::now()))
        })
        .await
        .ok()??;
        let delivered = tokio::time::timeout(timeout, rx.recv())
            .await
            .ok()
            .flatten()
            .map(|_| started.elapsed());
        drop(watcher);
        delivered
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

#[cfg(test)]
mod tests {
    use super::FsWatcher;
    use std::fs;
    use tempfile::TempDir;
    use tokio::time::{Duration, timeout};

    // These tests use FsWatcher::new_polling rather than FsWatcher::new so they
    // are decoupled from the host's kernel filesystem-notification stack. On
    // macOS, FSEvents can be disabled for processes without Full Disk Access (sandboxed
    // CI, editors without TCC prompts answered, terminal apps without the
    // entitlement). Polling works everywhere, at the cost of a short poll interval.
    // Production code still uses FsWatcher::new, which picks the kernel-native
    // backend (FSEvents on macOS, inotify on Linux).
    const POLL_INTERVAL: Duration = Duration::from_millis(100);

    // Polling fires a full directory scan on each tick; the first real event is
    // typically delivered within 2–3 poll intervals once a change has landed on
    // disk. A 5s outer timeout gives headroom on slow machines.
    const EVENT_TIMEOUT: Duration = Duration::from_secs(5);

    fn canonical_tempdir() -> (TempDir, std::path::PathBuf) {
        let tmp = TempDir::new().unwrap();
        let canonical = tmp.path().canonicalize().expect("canonicalize temp dir");
        (tmp, canonical)
    }

    #[tokio::test]
    async fn watcher_detects_file_creation() {
        let (_tmp, dir) = canonical_tempdir();
        let (mut watcher, mut rx) =
            FsWatcher::new_polling(POLL_INTERVAL).expect("Failed to create watcher");
        watcher.watch(&dir).expect("Failed to watch directory");

        fs::write(dir.join("test.txt"), "hello").unwrap();

        let event = timeout(EVENT_TIMEOUT, rx.recv()).await;
        assert!(event.is_ok(), "Should receive event within timeout");
        let paths = event.unwrap().unwrap();
        assert!(!paths.is_empty(), "Event should have affected paths");
    }

    #[tokio::test]
    async fn watcher_detects_file_modification() {
        let (_tmp, dir) = canonical_tempdir();
        let file_path = dir.join("test.txt");
        fs::write(&file_path, "initial").unwrap();

        let (mut watcher, mut rx) =
            FsWatcher::new_polling(POLL_INTERVAL).expect("Failed to create watcher");
        watcher.watch(&dir).expect("Failed to watch directory");

        // PollWatcher compares each scan against the previous one to detect
        // changes, and `watch()` takes that baseline snapshot synchronously before
        // it returns — so "initial" is already recorded here and the rewrite below
        // cannot race it. Nothing to wait for; wait on the event itself.
        fs::write(&file_path, "modified-distinct-content").unwrap();

        let paths = timeout(EVENT_TIMEOUT, rx.recv())
            .await
            .expect("Should receive modification event")
            .expect("watcher channel should stay open");
        assert!(
            paths.contains(&file_path),
            "modification event should name the modified file, got {paths:?}"
        );
    }

    #[tokio::test]
    async fn watcher_unwatch_stops_events() {
        let (_tmp, dir) = canonical_tempdir();
        let (_fence_tmp, fence_dir) = canonical_tempdir();
        let (mut watcher, mut rx) =
            FsWatcher::new_polling(POLL_INTERVAL).expect("Failed to create watcher");

        watcher.watch(&dir).expect("Failed to watch directory");
        watcher.unwatch(&dir).expect("Failed to unwatch");

        fs::write(dir.join("test.txt"), "hello").unwrap();

        // "No event ever arrives" has no condition to wait on, so fence it: one
        // poll tick scans every watched path, so the tick that reports the fence
        // dir's write is a tick that would already have reported `dir`'s — and
        // that event, being older, would sit ahead of the fence's in the queue.
        watcher
            .watch(&fence_dir)
            .expect("Failed to watch fence directory");
        fs::write(fence_dir.join("fence.txt"), "fence").unwrap();

        let paths = timeout(EVENT_TIMEOUT, rx.recv())
            .await
            .expect("fence event should arrive")
            .expect("watcher channel should stay open");
        assert!(
            paths.iter().all(|p| p.starts_with(&fence_dir)),
            "Should NOT receive events after unwatch; got {paths:?}"
        );
        while let Ok(paths) = rx.try_recv() {
            assert!(
                paths.iter().all(|p| p.starts_with(&fence_dir)),
                "Should NOT receive events after unwatch; got {paths:?}"
            );
        }
    }

    #[tokio::test]
    async fn watcher_multiple_paths() {
        let (_tmp1, dir1) = canonical_tempdir();
        let (_tmp2, dir2) = canonical_tempdir();

        let (mut watcher, mut rx) =
            FsWatcher::new_polling(POLL_INTERVAL).expect("Failed to create watcher");
        watcher.watch(&dir1).expect("Failed to watch dir 1");
        watcher.watch(&dir2).expect("Failed to watch dir 2");

        fs::write(dir1.join("a.txt"), "a").unwrap();

        let event = timeout(EVENT_TIMEOUT, rx.recv()).await;
        assert!(event.is_ok(), "Should receive event from first watched dir");
    }

    /// Diagnostic (ignored by default): measure fresh-stream FSEvents delivery
    /// latency at the locations that matter for the watch self-test. Run with:
    ///     cargo nextest run -E 'test(fs_event_delivery_by_location)' --run-ignored all --no-capture
    ///
    /// Interpreting output: notify requests FSEventStream latency 0.0 + NoDefer,
    /// so steady-state delivery should be milliseconds. A fresh stream also pays
    /// stream-startup cost (documented in benches/prompt_race.rs as reaching
    /// seconds). `TIMEOUT` means no delivery at all at that location —
    /// benches/prompt_race.rs records that FSEvents does not report for
    /// `$TMPDIR` (`/var/folders/...`).
    #[tokio::test]
    #[ignore]
    async fn fs_event_delivery_by_location() {
        let home = std::env::var("HOME").expect("HOME set");
        let darwin_tmp = std::process::Command::new("getconf")
            .arg("DARWIN_USER_TEMP_DIR")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty());

        let mut locations: Vec<(String, std::path::PathBuf)> = vec![
            ("env::temp_dir()".into(), std::env::temp_dir()),
            (
                "$HOME/.cache".into(),
                std::path::Path::new(&home).join(".cache"),
            ),
            ("/tmp".into(), "/tmp".into()),
        ];
        if let Some(d) = darwin_tmp {
            locations.push(("DARWIN_USER_TEMP_DIR".into(), d.into()));
        }

        for (label, base) in &locations {
            for i in 1..=3 {
                let result = super::self_test_native_backend_at(base, Duration::from_secs(3)).await;
                match result {
                    Some(d) => eprintln!("{label:>22} run {i}: delivered in {d:?}"),
                    None => eprintln!("{label:>22} run {i}: TIMEOUT (3s)"),
                }
            }
        }
    }
}
