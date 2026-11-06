use notify::event::{AccessKind, AccessMode};
use notify::{Config, Event, EventKind, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// The kernel's file watches, behind [`notify`]. Registering one can take
/// seconds (FSEvents on a loaded host), so the scheduler never calls it: its
/// [`Registrar`] does, on a thread of its own.
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

    /// A watcher over `watcher`, for a test that decides how a registration
    /// goes (one that blocks until released, or fails). Its events are the
    /// test's to send.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn with_backend(watcher: Box<dyn Watcher + Send>) -> Self {
        Self { watcher }
    }

    /// Watch `path` and everything below it. One that fails is taken down:
    /// inotify stops its walk at the first directory it may not read, and
    /// keeps the directories it had already added.
    pub fn watch(&mut self, path: &Path) -> notify::Result<()> {
        debug!("Watching: {:?}", path);
        let watched = self.watcher.watch(path, RecursiveMode::Recursive);
        if watched.is_err() {
            let _ = self.watcher.unwatch(path);
        }
        watched
    }

    /// Watch the entries of the directory `path`, and nothing below them.
    pub fn watch_entries(&mut self, path: &Path) -> notify::Result<()> {
        debug!("Watching the entries of: {:?}", path);
        self.watcher.watch(path, RecursiveMode::NonRecursive)
    }

    pub fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
        debug!("Unwatching: {:?}", path);
        self.watcher.unwatch(path)
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

/// How long a failed watch waits before a read asks for it again: a
/// registration can walk a whole tree, and a prompt reads many times a second.
pub const WATCH_RETRY_INTERVAL: Duration = Duration::from_secs(5);

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

/// How long a read waits for [`Barrier`] at most. FSEvents' own wake-up comes
/// by about 12 ms, so this is reached only when the machine is badly behind.
const BARRIER_WAIT: Duration = Duration::from_millis(20);

/// Events after the sentinel's that make the kernel hand FSEvents what it holds
/// at once, instead of on its 10 ms timer: it wakes `fseventsd` past 16 pending.
/// Should that ever change, a barrier takes the timer's 11 ms and is as right.
/// inotify queues an event before the write returns, and needs none.
const BARRIER_FILL: usize = if cfg!(target_os = "macos") { 17 } else { 0 };

/// Sentinels taken in turn, so a late or repeated event of one barrier's is not
/// taken for the next's.
const BARRIER_SENTINELS: usize = 8;

/// How a read knows every file event from before it has been heard. A watcher
/// delivers in order, so when a change the read made itself comes back, what
/// came before it has been through the scheduler. The change is to a file of
/// the daemon's, in a directory of its own.
pub struct Barrier {
    dir: PathBuf,
    sentinels: Vec<(PathBuf, std::ffi::CString)>,
    fill: Vec<std::ffi::CString>,
    /// The sentinel the barrier under way waits on.
    awaited: std::sync::atomic::AtomicUsize,
    heard: tokio::sync::watch::Sender<u64>,
    /// When the last barrier to complete began: a read that arrived before
    /// that shares it.
    last: tokio::sync::Mutex<Option<tokio::time::Instant>>,
    on: std::sync::atomic::AtomicBool,
}

fn touch(file: &std::ffi::CStr) {
    // SAFETY: a valid C string, and null asks for the time now.
    unsafe { libc::utimes(file.as_ptr(), std::ptr::null()) };
}

impl Barrier {
    /// Its files, in a directory of this process's. The caller watches
    /// [`Self::dir`] on the watcher whose events reach [`Self::hear`].
    pub fn new() -> std::io::Result<Self> {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        // `mkdtemp`: on a shared `/tmp` another user could lay symlinks in a
        // directory with a name they can guess, and the writes below follow them.
        let mut template = std::env::temp_dir()
            .join("comb-barrier-XXXXXX")
            .into_os_string()
            .into_vec();
        template.push(0);
        // SAFETY: a writable, NUL-terminated template, which it fills in place.
        if unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) }.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        template.pop();
        let dir = PathBuf::from(std::ffi::OsString::from_vec(template)).canonicalize()?;
        let file = |name: String| -> std::io::Result<(PathBuf, std::ffi::CString)> {
            let path = dir.join(name);
            std::fs::write(&path, b"")?;
            let c = std::ffi::CString::new(path.as_os_str().as_bytes())?;
            Ok((path, c))
        };
        let sentinels = (0..BARRIER_SENTINELS)
            .map(|i| file(format!("sentinel{i}")))
            .collect::<std::io::Result<_>>()?;
        let fill = (0..BARRIER_FILL)
            .map(|i| file(format!("fill{i}")).map(|(_, c)| c))
            .collect::<std::io::Result<_>>()?;
        Ok(Self {
            dir,
            sentinels,
            fill,
            awaited: std::sync::atomic::AtomicUsize::new(0),
            heard: tokio::sync::watch::Sender::new(0),
            last: tokio::sync::Mutex::new(None),
            on: std::sync::atomic::AtomicBool::new(true),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Take its files away, and with them any further wait on it: the daemon
    /// is stopping.
    pub fn remove(&self) {
        self.off();
        let _ = std::fs::remove_dir_all(&self.dir);
    }

    /// The watcher turned out not to deliver: no read waits on it again.
    pub fn off(&self) {
        self.on.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// Whether its own watch has delivered: until it has, a read does not wait.
    pub fn delivering(&self) -> bool {
        self.heard() > 0
    }

    /// How many of its own events have been heard: one more after a
    /// [`Self::pass`] is every event from before that pass heard.
    pub fn heard(&self) -> u64 {
        *self.heard.borrow()
    }

    /// The scheduler has been through `paths`, an event of its watcher.
    pub fn hear(&self, paths: &[PathBuf]) {
        let awaited = self.awaited.load(std::sync::atomic::Ordering::SeqCst);
        if paths.contains(&self.sentinels[awaited].0) {
            self.heard.send_modify(|n| *n += 1);
        }
    }

    /// Wait until the file events from before `arrived` have been heard, for
    /// [`BARRIER_WAIT`] at most. Until its own watch delivers, it only knocks.
    pub async fn pass(&self, arrived: tokio::time::Instant) {
        if !self.on.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let mut last = self.last.lock().await;
        if last.is_some_and(|began| began >= arrived) {
            return;
        }
        let began = tokio::time::Instant::now();
        let mut heard = self.heard.subscribe();
        let delivering = self.delivering();
        let awaited =
            (self.awaited.load(std::sync::atomic::Ordering::SeqCst) + 1) % BARRIER_SENTINELS;
        self.awaited
            .store(awaited, std::sync::atomic::Ordering::SeqCst);
        touch(&self.sentinels[awaited].1);
        if delivering {
            for file in &self.fill {
                touch(file);
            }
            let _ = tokio::time::timeout(BARRIER_WAIT, heard.changed()).await;
            *last = Some(began);
        }
    }
}

impl Drop for Barrier {
    fn drop(&mut self) {
        self.remove();
    }
}

/// Where one watched path stands, as `introspect watches` reports it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum WatchState {
    /// Asked for; the worker has not registered it yet.
    Pending,
    /// Registered: on the path itself, with everything below it, or, while
    /// the path does not exist, on the entries of `via`, the nearest directory
    /// above it that does. The path's creation is an event there.
    Active {
        #[serde(skip_serializing_if = "Option::is_none")]
        via: Option<PathBuf>,
    },
    /// The registration failed; the next demand of an instance that watches
    /// the path asks for it again.
    Failed { error: String },
}

/// One watched path and where it stands.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Watch {
    pub path: PathBuf,
    #[serde(flatten)]
    pub state: WatchState,
    /// Whole seconds since the path was last asked for: how long a `pending`
    /// one has been waiting on the worker.
    pub asked_secs_ago: u64,
}

/// The file watches the scheduler asks for, registered by a worker thread
/// that owns the [`FsWatcher`]. A registration can take seconds, and the
/// scheduler loop asks for one and goes on: a path is
/// [`WatchState::Pending`] until the worker has it.
///
/// Each path is registered once however many instances watch it; the
/// scheduler's own table routes its events to them.
pub struct Registrar {
    commands: std::sync::mpsc::Sender<Command>,
    table: Arc<Mutex<Table>>,
    settled: Option<mpsc::UnboundedReceiver<PathBuf>>,
    retry_after: Duration,
}

#[derive(Default)]
struct Table {
    /// Every path asked for: the ask that last asked for it, and where it
    /// stands. An ask the worker settles is written only while it is still
    /// the path's latest.
    /// When it was asked for rides along, for `introspect watches`.
    paths: BTreeMap<PathBuf, (u64, WatchState, Instant)>,
    asks: u64,
}

enum Command {
    Watch(PathBuf, u64),
    Unwatch(PathBuf),
    Backend(FsWatcher),
}

fn lock(table: &Mutex<Table>) -> MutexGuard<'_, Table> {
    table.lock().unwrap_or_else(|e| e.into_inner())
}

impl Registrar {
    /// Start the worker over `watcher`; a failed path is asked for again no
    /// sooner than `retry_after` after its last ask ([`WATCH_RETRY_INTERVAL`]).
    /// The worker ends when the registrar is dropped, after the registration
    /// under way, if any.
    pub fn start(watcher: FsWatcher, retry_after: Duration) -> Self {
        let (commands, rx) = std::sync::mpsc::channel();
        let table = Arc::new(Mutex::new(Table::default()));
        let (settled_tx, settled) = mpsc::unbounded_channel();
        let worker = Worker {
            watcher,
            table: Arc::clone(&table),
            settled: settled_tx,
            held: HashMap::new(),
            kernel: HashMap::new(),
        };
        std::thread::Builder::new()
            .name("comb-watches".into())
            .spawn(move || worker.run(rx))
            .expect("start the watch worker");
        Self {
            commands,
            table,
            settled: Some(settled),
            retry_after,
        }
    }

    /// Each path as its registration is made, from now on: events below it
    /// are delivered from then. Once.
    pub fn take_settled(&mut self) -> Option<mpsc::UnboundedReceiver<PathBuf>> {
        self.settled.take()
    }

    /// Whether `path`'s registration has been made, on the path itself.
    pub fn is_active(&self, path: &Path) -> bool {
        matches!(
            lock(&self.table).paths.get(path),
            Some((_, WatchState::Active { via: None }, _))
        )
    }

    /// Watch `path`: [`WatchState::Pending`] until the worker has registered
    /// it. Asking again for a path registers it afresh: one that failed tries
    /// again, and one watched through a directory above it looks for itself.
    pub fn watch(&self, path: PathBuf) {
        let ask = {
            let mut table = lock(&self.table);
            table.asks += 1;
            let ask = table.asks;
            table
                .paths
                .insert(path.clone(), (ask, WatchState::Pending, Instant::now()));
            ask
        };
        let _ = self.commands.send(Command::Watch(path, ask));
    }

    /// Stop watching `path`.
    pub fn unwatch(&self, path: &Path) {
        lock(&self.table).paths.remove(path);
        let _ = self.commands.send(Command::Unwatch(path.to_path_buf()));
    }

    /// Ask again for each of `paths` whose registration failed, once its
    /// last ask is `retry_after` old.
    pub fn retry<'a>(&self, paths: impl IntoIterator<Item = &'a Path>) {
        let failed: Vec<PathBuf> = {
            let table = lock(&self.table);
            paths
                .into_iter()
                .filter(|p| {
                    matches!(table.paths.get(*p), Some((_, WatchState::Failed { .. }, asked))
                        if asked.elapsed() >= self.retry_after)
                })
                .map(Path::to_path_buf)
                .collect()
        };
        for path in failed {
            self.watch(path);
        }
    }

    /// Whether any registration has failed: what a demand checks before it
    /// looks for the paths to [`Self::retry`].
    pub fn has_failed(&self) -> bool {
        lock(&self.table)
            .paths
            .values()
            .any(|(_, s, _)| matches!(s, WatchState::Failed { .. }))
    }

    /// `changed`, an entry of a directory a path is watched through, may be
    /// the path or a directory on the way to it: each such path is asked for
    /// again, so once it exists it is watched itself.
    pub fn look_again(&self, changed: &Path) {
        let Some(dir) = changed.parent() else {
            return;
        };
        let due: Vec<PathBuf> = lock(&self.table)
            .paths
            .iter()
            .filter(|(path, (_, state, _))| {
                matches!(state, WatchState::Active { via: Some(via) } if via == dir)
                    && path.starts_with(changed)
            })
            .map(|(path, _)| path.clone())
            .collect();
        for path in due {
            self.watch(path);
        }
    }

    /// Serve every watch from `watcher` from now on: the self-test's fallback
    /// to polling. The worker registers each one on it again.
    pub fn replace_backend(&self, watcher: FsWatcher) {
        let _ = self.commands.send(Command::Backend(watcher));
    }

    /// Every path asked for, and where it stands, by path.
    pub fn watches(&self) -> Vec<Watch> {
        lock(&self.table)
            .paths
            .iter()
            .map(|(path, (_, state, asked))| Watch {
                path: path.clone(),
                state: state.clone(),
                asked_secs_ago: asked.elapsed().as_secs(),
            })
            .collect()
    }
}

/// The thread that owns the [`FsWatcher`] and makes every registration.
struct Worker {
    watcher: FsWatcher,
    table: Arc<Mutex<Table>>,
    settled: mpsc::UnboundedSender<PathBuf>,
    /// The registration each path asked for rests on: itself, or the
    /// directory it is watched through.
    held: HashMap<PathBuf, PathBuf>,
    /// The registrations made, by path: whether below its entries too, and
    /// how many paths rest on it.
    kernel: HashMap<PathBuf, Kernel>,
}

struct Kernel {
    recursive: bool,
    users: usize,
}

impl Worker {
    fn run(mut self, commands: std::sync::mpsc::Receiver<Command>) {
        for command in commands {
            match command {
                Command::Watch(path, ask) => {
                    self.release(&path);
                    let state = self.register(&path);
                    if let WatchState::Failed { error } = &state {
                        warn!("Failed to watch {path:?}: {error}");
                    }
                    let mut table = lock(&self.table);
                    // Unwatched or asked for again since: a later command
                    // settles it.
                    if let Some((at, settled, _)) = table.paths.get_mut(&path)
                        && *at == ask
                    {
                        *settled = state;
                        let _ = self.settled.send(path);
                    }
                }
                Command::Unwatch(path) => self.release(&path),
                Command::Backend(watcher) => {
                    self.watcher = watcher;
                    for (path, kernel) in &self.kernel {
                        let registered = if kernel.recursive {
                            self.watcher.watch(path)
                        } else {
                            self.watcher.watch_entries(path)
                        };
                        if let Err(e) = registered {
                            warn!("failed to re-register watch on {path:?}: {e}");
                        }
                    }
                }
            }
        }
    }

    fn register(&mut self, path: &Path) -> WatchState {
        let (on, recursive) = match where_to_watch(path) {
            Ok(found) => found,
            Err(error) => return WatchState::Failed { error },
        };
        if let Err(e) = self.acquire(&on, recursive) {
            return WatchState::Failed {
                error: e.to_string(),
            };
        }
        let via = (on != path).then(|| on.clone());
        self.held.insert(path.to_path_buf(), on);
        WatchState::Active { via }
    }

    /// One more path resting on the registration of `on`, made now if it is
    /// the first. One that only watched entries is made again over the whole
    /// tree when a path needs that.
    fn acquire(&mut self, on: &Path, recursive: bool) -> notify::Result<()> {
        if let Some(kernel) = self.kernel.get_mut(on) {
            if recursive && !kernel.recursive {
                let _ = self.watcher.unwatch(on);
                if let Err(e) = self.watcher.watch(on) {
                    // The paths that rest on it keep what they had.
                    let _ = self.watcher.watch_entries(on);
                    return Err(e);
                }
                kernel.recursive = true;
            }
            kernel.users += 1;
            return Ok(());
        }
        if recursive {
            self.watcher.watch(on)?;
        } else {
            self.watcher.watch_entries(on)?;
        }
        self.kernel.insert(
            on.to_path_buf(),
            Kernel {
                recursive,
                users: 1,
            },
        );
        Ok(())
    }

    /// `path` no longer rests on its registration; the last path to leave one
    /// takes it down.
    fn release(&mut self, path: &Path) {
        let Some(on) = self.held.remove(path) else {
            return;
        };
        if let Some(kernel) = self.kernel.get_mut(&on) {
            kernel.users -= 1;
            if kernel.users == 0 {
                self.kernel.remove(&on);
                let _ = self.watcher.unwatch(&on);
                debug!("Unwatched {on:?}");
            }
        }
    }
}

/// Where a watch of `path` is registered, and whether over its whole tree:
/// on the path itself when it exists; while it does not, on the entries of
/// the nearest directory above it that does. `/` and `$HOME` are watched on
/// their entries only, whatever asked for them: their trees are everything.
fn where_to_watch(path: &Path) -> Result<(PathBuf, bool), String> {
    match std::fs::metadata(path) {
        Ok(_) => {
            let real = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
            let everything =
                real == Path::new("/") || crate::query::home_dir().is_some_and(|h| h == real);
            Ok((path.to_path_buf(), !everything))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut up = path.parent();
            while let Some(dir) = up {
                match std::fs::metadata(dir) {
                    Ok(meta) if meta.is_dir() => return Ok((dir.to_path_buf(), false)),
                    Ok(_) => return Err(format!("{} is not a directory", dir.display())),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => up = dir.parent(),
                    Err(e) => return Err(format!("{}: {e}", dir.display())),
                }
            }
            Err("no directory above it exists".into())
        }
        Err(e) => Err(e.to_string()),
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

/// The handler every backend calls with each event: the paths of a change,
/// handed to the scheduler. A read is dropped ([`is_change`]).
fn event_handler(
    tx: mpsc::Sender<Vec<std::path::PathBuf>>,
) -> impl Fn(Result<Event, notify::Error>) + Send + 'static {
    move |result| match result {
        Ok(event) => {
            if is_change(&event.kind) && !event.paths.is_empty() {
                let _ = tx.blocking_send(event.paths);
            }
        }
        Err(e) => {
            warn!("Filesystem watch error: {}", e);
        }
    }
}

/// Whether an event is a change: anything but a read. inotify reports every
/// open, so a Source reading the files it watches would refresh itself without
/// end, and a recursive watch's own walk reports each directory it opens. A
/// close after writing is a change (an `mmap` write reports nothing else).
fn is_change(kind: &EventKind) -> bool {
    !matches!(kind, EventKind::Access(access) if !matches!(access, AccessKind::Close(AccessMode::Write)))
}

#[cfg(test)]
mod barrier_tests {
    use super::*;

    fn delivering() -> Barrier {
        let barrier = Barrier::new().expect("a barrier");
        barrier.heard.send_modify(|n| *n += 1);
        barrier
    }

    async fn pending(pass: &mut (impl std::future::Future<Output = ()> + Unpin)) -> bool {
        tokio::select! {
            biased;
            _ = pass => false,
            _ = std::future::ready(()) => true,
        }
    }

    /// What its own sentinel is, once a pass has touched it.
    fn awaited(barrier: &Barrier) -> Vec<PathBuf> {
        let at = barrier.awaited.load(std::sync::atomic::Ordering::SeqCst);
        vec![barrier.sentinels[at].0.clone()]
    }

    #[tokio::test(start_paused = true)]
    async fn a_read_passes_when_its_own_sentinel_is_heard() {
        let barrier = delivering();
        let began = tokio::time::Instant::now();
        let pass = barrier.pass(began);
        tokio::pin!(pass);
        assert!(pending(&mut pass).await);
        // A late event of the barrier before it is not its answer.
        let other = (barrier.awaited.load(std::sync::atomic::Ordering::SeqCst) + BARRIER_SENTINELS
            - 1)
            % BARRIER_SENTINELS;
        barrier.hear(&[barrier.sentinels[other].0.clone()]);
        assert!(pending(&mut pass).await);
        barrier.hear(&awaited(&barrier));
        pass.await;
        assert_eq!(tokio::time::Instant::now(), began);
    }

    #[tokio::test(start_paused = true)]
    async fn a_read_stops_waiting_at_the_limit() {
        let barrier = delivering();
        let began = tokio::time::Instant::now();
        barrier.pass(began).await;
        assert_eq!(began.elapsed(), BARRIER_WAIT);
    }

    #[tokio::test(start_paused = true)]
    async fn a_read_from_before_a_barrier_shares_it_and_a_later_one_does_not() {
        let barrier = delivering();
        let first = tokio::time::Instant::now();
        barrier.pass(first).await;
        // It began after `first` arrived, so `first` needs no other.
        let at = barrier.awaited.load(std::sync::atomic::Ordering::SeqCst);
        barrier.pass(first).await;
        assert_eq!(
            barrier.awaited.load(std::sync::atomic::Ordering::SeqCst),
            at
        );
        tokio::time::advance(Duration::from_millis(1)).await;
        barrier.pass(tokio::time::Instant::now()).await;
        assert_ne!(
            barrier.awaited.load(std::sync::atomic::Ordering::SeqCst),
            at
        );
    }

    #[tokio::test]
    async fn off_or_before_it_delivers_a_read_does_not_wait() {
        let barrier = Barrier::new().expect("a barrier");
        barrier.pass(tokio::time::Instant::now()).await;
        let barrier = delivering();
        barrier.off();
        let began = std::time::Instant::now();
        barrier.pass(tokio::time::Instant::now()).await;
        assert!(began.elapsed() < BARRIER_WAIT);
    }
}

/// A backend for tests: each registration waits until the test opens its
/// gate, and every call is recorded.
#[cfg(test)]
pub(crate) mod test_backend {
    use notify::{RecursiveMode, Watcher};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Condvar, Mutex};

    /// What the backend was asked to do.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Call {
        Watch(PathBuf, RecursiveMode),
        Unwatch(PathBuf),
    }

    #[derive(Default)]
    pub struct Gate {
        open: Mutex<bool>,
        opened: Condvar,
    }

    impl Gate {
        pub fn closed() -> Arc<Self> {
            Arc::default()
        }

        pub fn opened() -> Arc<Self> {
            let gate = Self::closed();
            gate.open();
            gate
        }

        pub fn open(&self) {
            *self.open.lock().unwrap() = true;
            self.opened.notify_all();
        }

        fn wait(&self) {
            let mut open = self.open.lock().unwrap();
            while !*open {
                open = self.opened.wait(open).unwrap();
            }
        }
    }

    pub struct Held {
        pub gate: Arc<Gate>,
        pub calls: Arc<Mutex<Vec<Call>>>,
        /// Fail every recursive watch, after recording it.
        pub refuse_trees: bool,
    }

    impl Held {
        pub fn new(gate: &Arc<Gate>) -> (Self, Arc<Mutex<Vec<Call>>>) {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let held = Self {
                gate: Arc::clone(gate),
                calls: Arc::clone(&calls),
                refuse_trees: false,
            };
            (held, calls)
        }
    }

    impl Watcher for Held {
        fn new<F: notify::EventHandler>(_: F, _: notify::Config) -> notify::Result<Self> {
            Err(notify::Error::generic(
                "a test backend is made by Held::new",
            ))
        }

        fn watch(&mut self, path: &Path, mode: RecursiveMode) -> notify::Result<()> {
            self.gate.wait();
            self.calls
                .lock()
                .unwrap()
                .push(Call::Watch(path.to_path_buf(), mode));
            if self.refuse_trees && mode == RecursiveMode::Recursive {
                return Err(notify::Error::generic("refused"));
            }
            Ok(())
        }

        fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(Call::Unwatch(path.to_path_buf()));
            Ok(())
        }

        fn kind() -> notify::WatcherKind {
            notify::WatcherKind::NullWatcher
        }
    }
}

#[cfg(test)]
mod registrar_tests {
    use super::test_backend::{Call, Gate, Held};
    use super::*;
    use notify::RecursiveMode;

    fn registrar(gate: &Arc<Gate>) -> (Registrar, Arc<Mutex<Vec<Call>>>) {
        let (held, calls) = Held::new(gate);
        (
            Registrar::start(FsWatcher::with_backend(Box::new(held)), Duration::ZERO),
            calls,
        )
    }

    /// A registrar whose backend fails every recursive watch.
    fn refusing(retry_after: Duration) -> (Registrar, Arc<Mutex<Vec<Call>>>) {
        let (mut held, calls) = Held::new(&Gate::opened());
        held.refuse_trees = true;
        (
            Registrar::start(FsWatcher::with_backend(Box::new(held)), retry_after),
            calls,
        )
    }

    fn until_failed(r: &Registrar) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !r.has_failed() {
            assert!(std::time::Instant::now() < deadline, "never failed");
            std::thread::yield_now();
        }
    }

    #[test]
    fn root_and_home_are_watched_on_their_entries_only() {
        let (r, calls) = registrar(&Gate::opened());
        let home = crate::query::home_dir().filter(|h| h.is_dir());
        let wide: Vec<PathBuf> = std::iter::once(PathBuf::from("/")).chain(home).collect();
        for path in &wide {
            r.watch(path.clone());
            until(&r, path, WatchState::Active { via: None });
        }
        let want: Vec<Call> = wide
            .iter()
            .map(|p| Call::Watch(p.clone(), RecursiveMode::NonRecursive))
            .collect();
        assert_eq!(*calls.lock().unwrap(), want);
    }

    /// A recursive watch that fails partway leaves directories registered;
    /// the failure takes them down.
    #[test]
    fn a_failed_tree_watch_is_taken_down() {
        let (_tmp, dir) = tempdir();
        let (r, calls) = refusing(Duration::ZERO);
        r.watch(dir.clone());
        until_failed(&r);
        assert_eq!(
            *calls.lock().unwrap(),
            [
                Call::Watch(dir.clone(), RecursiveMode::Recursive),
                Call::Unwatch(dir.clone()),
            ]
        );
    }

    /// A failed path is asked for again only once its last ask is old enough.
    #[test]
    fn a_failed_path_is_retried_only_after_the_interval() {
        let attempts = |calls: &Mutex<Vec<Call>>| {
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|c| matches!(c, Call::Watch(..)))
                .count()
        };
        let (_tmp, dir) = tempdir();
        let (r, calls) = refusing(Duration::from_secs(3600));
        r.watch(dir.clone());
        until_failed(&r);
        r.retry([dir.as_path()]);
        r.retry([dir.as_path()]);
        assert!(r.has_failed(), "neither retry asked again");
        assert_eq!(attempts(&calls), 1);

        let (r, calls) = refusing(Duration::ZERO);
        r.watch(dir.clone());
        until_failed(&r);
        r.retry([dir.as_path()]);
        until_failed(&r);
        assert_eq!(attempts(&calls), 2);
    }

    fn state(r: &Registrar, path: &Path) -> Option<WatchState> {
        r.watches()
            .into_iter()
            .find(|w| w.path == path)
            .map(|w| w.state)
    }

    /// Wait for the worker to settle `path` as `want`: it has no signal of
    /// its own, so this spins, yielding, with a deadline.
    fn until(r: &Registrar, path: &Path, want: WatchState) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while state(r, path).as_ref() != Some(&want) {
            assert!(
                std::time::Instant::now() < deadline,
                "{path:?} never reached {want:?}: {:?}",
                state(r, path)
            );
            std::thread::yield_now();
        }
    }

    fn tempdir() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        (tmp, dir)
    }

    #[test]
    fn a_path_is_pending_until_its_registration_is_made() {
        let (_tmp, dir) = tempdir();
        let gate = Gate::closed();
        let (r, calls) = registrar(&gate);
        r.watch(dir.clone());
        assert_eq!(state(&r, &dir), Some(WatchState::Pending));
        gate.open();
        until(&r, &dir, WatchState::Active { via: None });
        assert_eq!(
            *calls.lock().unwrap(),
            [Call::Watch(dir.clone(), RecursiveMode::Recursive)]
        );
    }

    #[test]
    fn an_absent_path_is_watched_through_the_nearest_directory_that_exists() {
        let (_tmp, dir) = tempdir();
        let path = dir.join("a/b/state.json");
        let (r, calls) = registrar(&Gate::opened());
        r.watch(path.clone());
        until(
            &r,
            &path,
            WatchState::Active {
                via: Some(dir.clone()),
            },
        );
        assert_eq!(
            *calls.lock().unwrap(),
            [Call::Watch(dir.clone(), RecursiveMode::NonRecursive)],
            "only the entries of the directory that exists, not its tree"
        );
    }

    /// Once the path exists, an event naming it (or a directory on the way to
    /// it) moves its watch onto it, and the directory above is let go.
    #[test]
    fn a_created_path_is_watched_itself_when_looked_at_again() {
        let (_tmp, dir) = tempdir();
        let path = dir.join("state.json");
        let (r, calls) = registrar(&Gate::opened());
        r.watch(path.clone());
        until(
            &r,
            &path,
            WatchState::Active {
                via: Some(dir.clone()),
            },
        );
        r.look_again(&dir.join("other"));
        std::fs::write(&path, "{}").unwrap();
        r.look_again(&path);
        until(&r, &path, WatchState::Active { via: None });
        assert_eq!(
            *calls.lock().unwrap(),
            [
                Call::Watch(dir.clone(), RecursiveMode::NonRecursive),
                Call::Unwatch(dir.clone()),
                Call::Watch(path.clone(), RecursiveMode::Recursive),
            ],
            "an event on an unrelated entry does not count"
        );
    }

    #[test]
    fn paths_through_one_directory_share_its_registration() {
        let (_tmp, dir) = tempdir();
        let (x, y) = (dir.join("x"), dir.join("y"));
        let (r, calls) = registrar(&Gate::opened());
        r.watch(x.clone());
        r.watch(y.clone());
        let through = WatchState::Active {
            via: Some(dir.clone()),
        };
        until(&r, &y, through.clone());
        r.unwatch(&x);
        // A fence: the worker takes commands in order, so once this settles
        // the unwatch before it has been done.
        r.watch(dir.clone());
        until(&r, &dir, WatchState::Active { via: None });
        assert_eq!(
            *calls.lock().unwrap(),
            [
                Call::Watch(dir.clone(), RecursiveMode::NonRecursive),
                Call::Unwatch(dir.clone()),
                Call::Watch(dir.clone(), RecursiveMode::Recursive),
            ],
            "one registration for both; y keeps it when x goes, and a path that \
             needs the whole tree makes it again over the tree"
        );
        assert_eq!(state(&r, &x), None);
    }

    #[test]
    fn a_path_that_cannot_be_read_fails_and_is_retried() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, dir) = tempdir();
        let locked = dir.join("locked");
        std::fs::create_dir(&locked).unwrap();
        let path = locked.join("state.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::metadata(&path).is_ok() {
            // Root reads through any mode: nothing here can fail.
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }
        let (r, calls) = registrar(&Gate::opened());
        r.watch(path.clone());
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !r.has_failed() {
            assert!(std::time::Instant::now() < deadline, "never failed");
            std::thread::yield_now();
        }
        let Some(WatchState::Failed { error }) = state(&r, &path) else {
            panic!("{:?}", state(&r, &path));
        };
        assert!(error.contains("ermission denied"), "{error}");
        assert!(calls.lock().unwrap().is_empty(), "nothing was registered");

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        r.retry([dir.as_path(), path.as_path()]);
        until(&r, &path, WatchState::Active { via: None });
        assert_eq!(state(&r, &dir), None, "retry asks only for a failed path");
    }

    #[test]
    fn a_new_backend_takes_every_registration() {
        let (_tmp, dir) = tempdir();
        let absent = dir.join("absent");
        let (r, _) = registrar(&Gate::opened());
        r.watch(dir.clone());
        r.watch(absent.clone());
        until(
            &r,
            &absent,
            WatchState::Active {
                via: Some(dir.clone()),
            },
        );
        let (held, calls) = Held::new(&Gate::opened());
        r.replace_backend(FsWatcher::with_backend(Box::new(held)));
        let fence = dir.join("fence");
        r.watch(fence.clone());
        until(
            &r,
            &fence,
            WatchState::Active {
                via: Some(dir.clone()),
            },
        );
        assert_eq!(
            *calls.lock().unwrap(),
            [Call::Watch(dir.clone(), RecursiveMode::Recursive)],
            "the one registration both rest on, made again on the new backend"
        );
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

    /// A read is not handed to the scheduler; a change is, a close after
    /// writing included. A Source that read the files it watched refreshed
    /// itself without end on Linux, where inotify reports every open.
    #[test]
    fn the_handler_drops_reads() {
        use notify::event::{AccessKind, AccessMode, CreateKind, DataChange, ModifyKind};
        use notify::{Event, EventKind};
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let handle = super::event_handler(tx);
        let event = |kind, path: &str| Ok(Event::new(kind).add_path(path.into()));
        handle(event(
            EventKind::Access(AccessKind::Open(AccessMode::Any)),
            "/r/open",
        ));
        handle(event(EventKind::Access(AccessKind::Read), "/r/read"));
        handle(event(
            EventKind::Access(AccessKind::Close(AccessMode::Read)),
            "/r/close",
        ));
        handle(event(
            EventKind::Modify(ModifyKind::Data(DataChange::Any)),
            "/c/modify",
        ));
        handle(event(EventKind::Create(CreateKind::File), "/c/create"));
        handle(event(
            EventKind::Access(AccessKind::Close(AccessMode::Write)),
            "/c/written",
        ));

        let mut paths = Vec::new();
        while let Ok(batch) = rx.try_recv() {
            paths.extend(batch);
        }
        let expected: Vec<std::path::PathBuf> = ["/c/modify", "/c/create", "/c/written"]
            .iter()
            .map(Into::into)
            .collect();
        assert_eq!(paths, expected);
    }

    /// The same through inotify: reading a watched file reports nothing.
    /// `git.head`, rereading `.git/HEAD`, ran about 2,500 times a second.
    /// FSEvents reports no reads, so this runs where inotify does.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_read_is_not_reported_as_a_change() {
        let (_tmp, dir) = canonical_tempdir();
        let read = dir.join("read");
        let fence = dir.join("fence");
        fs::write(&read, "x").unwrap();
        let (mut watcher, mut rx) = FsWatcher::new().expect("Failed to create watcher");
        watcher.watch(&dir).expect("Failed to watch directory");

        assert_eq!(fs::read_to_string(&read).unwrap(), "x");
        fs::write(&fence, "f").unwrap();

        let mut seen = Vec::new();
        while !seen.contains(&fence) {
            let paths = timeout(EVENT_TIMEOUT, rx.recv())
                .await
                .expect("fence event should arrive")
                .expect("watcher channel should stay open");
            seen.extend(paths);
        }
        assert!(
            !seen.contains(&read),
            "reading a file reported it as changed: {seen:?}"
        );
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
