use crate::cache::Cache;
use crate::config::Config;
use crate::provider::registry::ProviderRegistry;
use crate::server::Server;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn};

pub fn pid_path_for_socket(socket_path: &Path) -> PathBuf {
    socket_path.with_file_name("daemon.pid")
}

pub fn is_daemon_running(socket_path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket_path).is_ok()
}

pub fn start_in_process(
    socket_path: PathBuf,
    config: Config,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        run_daemon(socket_path, config).await;
    })
}

async fn run_daemon(socket_path: PathBuf, _config: Config) {
    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());

    compute_once_providers(&registry, &cache);

    let server = Server::new(socket_path, cache, registry);
    if let Err(e) = server.run().await {
        tracing::error!("Server error: {}", e);
    }
}

fn compute_once_providers(registry: &ProviderRegistry, cache: &Cache) {
    for name in registry.list() {
        if let Some(provider) = registry.get(&name) {
            let meta = provider.metadata();
            if matches!(meta.invalidation, crate::provider::InvalidationStrategy::Once) {
                match provider.execute(None) {
                    Some(result) => {
                        cache.put(&name, None, result);
                        info!("Computed initial value for provider '{}'", name);
                    }
                    None => {
                        warn!("Provider '{}' returned None during initial computation", name);
                    }
                }
            }
        }
    }
}

pub fn fork_daemon(binary_path: &str, socket_path: &Path) -> std::io::Result<()> {
    use std::process::Command;

    let pid_path = pid_path_for_socket(socket_path);

    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let child = Command::new(binary_path)
        .arg("daemon")
        .arg("--socket")
        .arg(socket_path.as_os_str())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    std::fs::write(&pid_path, child.id().to_string())?;

    Ok(())
}

pub fn wait_for_daemon(socket_path: &Path, max_attempts: u32) -> bool {
    let mut delay_ms = 10u64;
    for _ in 0..max_attempts {
        if is_daemon_running(socket_path) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        delay_ms = (delay_ms * 2).min(500);
    }
    false
}

pub fn ensure_daemon(socket_path: &Path) -> std::io::Result<()> {
    if is_daemon_running(socket_path) {
        return Ok(());
    }

    let binary = std::env::current_exe()?
        .to_string_lossy()
        .to_string();

    fork_daemon(&binary, socket_path)?;

    if !wait_for_daemon(socket_path, 8) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "Daemon failed to start within timeout",
        ));
    }

    Ok(())
}
