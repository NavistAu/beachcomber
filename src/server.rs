//! The daemon's socket server: the accept loop here, a module per op.

mod answer;
mod cache_facet;
mod connection;
mod introspect;
mod put;
mod read;
mod refresh;
mod status;
mod watch;

use crate::boundaries::proc_snap::{ProcessSnapshotter, RealProcessSnapshotter};
use crate::cache::Cache;
use crate::config::Config;
use crate::provider::registry::RegistryHandle;
use crate::scheduler::SchedulerHandle;
use crate::watcher_registry::WatcherRegistry;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::time::{Duration, Instant};
use tokio::net::UnixListener;
use tracing::{debug, info, warn};

pub(crate) use put::put;

/// How long the accept loop waits after a failed accept. Out of descriptors, a
/// pending connection stays readable (on Linux; macOS drops it), so without a
/// wait the next try fails at once, for as long as none is free.
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

/// What every connection and every op handler shares. Cloning it is cheap:
/// every field is a handle or a small value.
#[derive(Clone)]
struct Ctx {
    socket_path: PathBuf,
    cache: Arc<Cache>,
    registry: RegistryHandle,
    scheduler: SchedulerHandle,
    watchers: Arc<WatcherRegistry>,
    start_instant: Instant,
    requests_total: Arc<AtomicU64>,
    /// Open `watch` streams, one per client, counted while each is streaming.
    /// Not the watcher registry's entry count: one watch subscribes to every
    /// entry on its path's chain, so that would report a single watch at depth
    /// six as six.
    active_watches: Arc<AtomicUsize>,
    config: Arc<Config>,
    /// Reaper health, present only in the daemon path. None for
    /// embedded/test servers: introspect then omits the field.
    reaper: Option<Arc<crate::singleton::ReaperHealth>>,
    /// What `introspect procs` samples process starts through: the system's
    /// tracer, or a test's stand-in ([`Server::with_process_snapshotter`]).
    procs: Arc<dyn ProcessSnapshotter>,
}

pub struct Server {
    ctx: Ctx,
}

impl Server {
    /// A server on the default configuration, which reads no file: an
    /// embedded or test server. The daemon passes its own
    /// ([`Self::new_with_config`]).
    pub fn new(
        socket_path: PathBuf,
        cache: Arc<Cache>,
        registry: impl Into<RegistryHandle>,
        scheduler: SchedulerHandle,
        watchers: Arc<WatcherRegistry>,
    ) -> Self {
        Self::new_with_config(
            socket_path,
            cache,
            registry,
            scheduler,
            watchers,
            Config::default(),
        )
    }

    pub fn new_with_config(
        socket_path: PathBuf,
        cache: Arc<Cache>,
        registry: impl Into<RegistryHandle>,
        scheduler: SchedulerHandle,
        watchers: Arc<WatcherRegistry>,
        config: Config,
    ) -> Self {
        Self {
            ctx: Ctx {
                socket_path,
                cache,
                registry: registry.into(),
                scheduler,
                watchers,
                start_instant: Instant::now(),
                requests_total: Arc::new(AtomicU64::new(0)),
                active_watches: Arc::new(AtomicUsize::new(0)),
                config: Arc::new(config),
                reaper: None,
                procs: Arc::new(RealProcessSnapshotter),
            },
        }
    }

    /// Attach reaper health state for introspect surfacing (daemon path only).
    pub fn with_reaper_health(mut self, health: Arc<crate::singleton::ReaperHealth>) -> Self {
        self.ctx.reaper = Some(health);
        self
    }

    /// Sample process starts for `introspect procs` through `procs` instead
    /// of the system's tracer.
    pub fn with_process_snapshotter(mut self, procs: Arc<dyn ProcessSnapshotter>) -> Self {
        self.ctx.procs = procs;
        self
    }

    pub async fn run(&self) -> std::io::Result<()> {
        let listener = self.bind()?;
        self.serve(listener).await
    }

    /// Bind the socket and start listening. Once this returns, clients can
    /// connect — connections queue in the backlog until [`Server::serve`]
    /// accepts them — so a caller that binds first never has to guess when the
    /// socket is live.
    pub fn bind(&self) -> std::io::Result<UnixListener> {
        if let Some(parent) = self.ctx.socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Clean up stale socket file. If another daemon is actively listening,
        // the bind will fail with EADDRINUSE — that's correct behavior.
        if self.ctx.socket_path.exists() {
            // Check if something is actually listening
            if std::os::unix::net::UnixStream::connect(&self.ctx.socket_path).is_ok() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!(
                        "Another daemon is already listening on {:?}",
                        self.ctx.socket_path
                    ),
                ));
            }
            // Stale socket file — remove it
            let _ = std::fs::remove_file(&self.ctx.socket_path);
        }

        let listener = UnixListener::bind(&self.ctx.socket_path)?;
        info!("Listening on {:?}", self.ctx.socket_path);
        Ok(listener)
    }

    /// Accept loop over a listener from [`Server::bind`]. An accept that fails
    /// is tried again every [`ACCEPT_RETRY`]; a run of failures logs once.
    pub async fn serve(&self, listener: UnixListener) -> std::io::Result<()> {
        let mut failing = false;
        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    if std::mem::take(&mut failing) {
                        info!("Accepting connections again");
                    }
                    let ctx = self.ctx.clone();
                    tokio::spawn(async move {
                        if let Err(e) = connection::handle_connection(stream, ctx).await {
                            debug!("Connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    if !std::mem::replace(&mut failing, true) {
                        warn!(
                            "Accept error: {e}; trying again every {ACCEPT_RETRY:?} until one works"
                        );
                    }
                    tokio::time::sleep(ACCEPT_RETRY).await;
                }
            }
        }
    }

    /// Bind, then run the accept loop on its own task. The socket is
    /// connectable when this returns, so a caller can connect immediately
    /// rather than sleeping and hoping.
    pub fn spawn(self) -> std::io::Result<tokio::task::JoinHandle<std::io::Result<()>>> {
        let listener = self.bind()?;
        Ok(tokio::spawn(async move { self.serve(listener).await }))
    }
}
