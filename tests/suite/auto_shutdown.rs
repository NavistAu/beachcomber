use beachcomber::cache::Cache;
use beachcomber::config::Config;
use beachcomber::provider::Value;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::scheduler::Scheduler;
use beachcomber::watcher_registry::WatcherRegistry;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

/// On paused time: the second of idleness and the tick that notices it
/// elapse as soon as the runtime has nothing else to do.
#[tokio::test(start_paused = true)]
async fn scheduler_shuts_down_when_idle() {
    let cache = Arc::new(Cache::new());
    // Empty registry — no Once providers to compute
    let registry = Arc::new(ProviderRegistry::new());

    // Shut down after 1s idle.
    let config = Config::from_toml("[daemon]\nidle_shutdown = \"1s\"\n");

    assert!(shuts_down_within(cache, registry, config, 5).await);
}

/// Whether a scheduler over `cache` stops on its own within `secs`, and not
/// before the 1 s `idle_shutdown` the configs here set.
async fn shuts_down_within(
    cache: Arc<Cache>,
    registry: Arc<ProviderRegistry>,
    config: Config,
    secs: u64,
) -> bool {
    let (_handle, scheduler) =
        Scheduler::new(cache, registry, config, Arc::new(WatcherRegistry::new()));
    let started = tokio::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(secs),
        tokio::spawn(async move { scheduler.run().await }),
    )
    .await;
    if result.is_ok() {
        assert!(
            started.elapsed() >= Duration::from_secs(1),
            "not before the idle timeout: {:?}",
            started.elapsed()
        );
    }
    result.is_ok()
}

/// A cache seeded as a daemon seeds it at start: the managed defaults and a
/// `config.toml` setting `idle_shutdown = "1s"`, under a `$HOME` and
/// `$XDG_CONFIG_HOME` of the test's own (nextest runs each test in its own
/// process). The directories must outlive the test.
fn seeded() -> (TempDir, TempDir, Arc<Cache>, Arc<ProviderRegistry>, Config) {
    let home = TempDir::new().unwrap();
    let xdg = TempDir::new().unwrap();
    // SAFETY: nextest gives every test its own process, so no other thread is
    // reading the environment concurrently.
    unsafe { std::env::set_var("HOME", home.path().canonicalize().unwrap()) };
    unsafe { std::env::set_var("XDG_CONFIG_HOME", xdg.path()) };
    let dir = xdg.path().join("beachcomber");
    beachcomber::daemon::defaults::write_managed_defaults(&dir.join("defaults"), "9.9.9").unwrap();
    std::fs::write(
        dir.join("config.toml"),
        "[daemon]\nidle_shutdown = \"1s\"\n",
    )
    .unwrap();

    let cache = Arc::new(Cache::new());
    let config = Config::load();
    let load = ProviderRegistry::load(&config, &config.vars());
    beachcomber::daemon::seed::seed_facets(&cache, &load.registry, load.seed);
    assert!(!cache.is_empty(), "the defaults seed records");
    (home, xdg, cache, Arc::new(load.registry), config)
}

/// The records a daemon seeds from its own configuration do not keep it up.
#[tokio::test(start_paused = true)]
async fn a_daemon_seeded_from_its_config_still_shuts_down_when_idle() {
    let (_home, _xdg, cache, registry, config) = seeded();
    assert!(shuts_down_within(cache, registry, config, 5).await);
}

/// A put, with no ttl and so no lifecycle, keeps the daemon up.
#[tokio::test(start_paused = true)]
async fn a_put_keeps_an_idle_daemon_up() {
    let (_home, _xdg, cache, registry, config) = seeded();
    cache
        .write_static(
            "myapp",
            None,
            &beachcomber::cache::Origin::Put,
            "x",
            Value::Int(1),
            None,
        )
        .unwrap();
    assert!(!shuts_down_within(cache, registry, config, 5).await);
}
