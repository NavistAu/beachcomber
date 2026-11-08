//! A [`Server`] with a running scheduler, for tests that exercise the server
//! alone. A server cannot be built without one. Call this inside a tokio
//! runtime (the scheduler is spawned on it).

use beachcomber::cache::Cache;
use beachcomber::config::Config;
use beachcomber::provider::registry::RegistryHandle;
use beachcomber::scheduler::Scheduler;
use beachcomber::server::Server;
use beachcomber::watcher_registry::WatcherRegistry;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// `Server::new` over `cache` and `registry`, with a scheduler on its default
/// configuration. A polling watcher, which works where the system's does not.
pub fn server(
    socket_path: PathBuf,
    cache: Arc<Cache>,
    registry: impl Into<RegistryHandle>,
    watchers: Arc<WatcherRegistry>,
) -> Server {
    let registry: RegistryHandle = registry.into();
    let (handle, mut scheduler) = Scheduler::new(
        cache.clone(),
        registry.current(),
        Config::default(),
        watchers.clone(),
    );
    scheduler.use_polling_watcher(Duration::from_millis(50));
    tokio::spawn(scheduler.run());
    Server::new(socket_path, cache, registry, handle, watchers)
}

/// A scheduler handle whose queue is drained and ignored: nothing polls,
/// watches or runs on demand, and its status never answers. For tests whose
/// point is a read with no scheduler to help it.
pub fn idle() -> beachcomber::scheduler::SchedulerHandle {
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    beachcomber::scheduler::SchedulerHandle::new(tx)
}

/// [`server`] over an [`idle`] scheduler.
pub fn idle_server(
    socket_path: PathBuf,
    cache: Arc<Cache>,
    registry: impl Into<RegistryHandle>,
    watchers: Arc<WatcherRegistry>,
) -> Server {
    Server::new(socket_path, cache, registry, idle(), watchers)
}
