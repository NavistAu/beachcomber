use beachcomber::cache::Cache;
use beachcomber::client::Client;
use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::scheduler::Scheduler;
use beachcomber::server::Server;
use std::process::Command;
use std::sync::Arc;
use tempfile::TempDir;

fn create_test_repo() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::fs::write(dir.join("README.md"), "# test").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(dir)
        .output()
        .unwrap();
    tmp
}

async fn setup_daemon() -> (
    TempDir,
    std::path::PathBuf,
    beachcomber::scheduler::SchedulerHandle,
) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let config = Config::default();

    let (handle, scheduler) = Scheduler::new(cache.clone(), registry.clone(), config, watchers.clone());
    tokio::spawn(async move { scheduler.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let server = Server::new(
        sock.clone(),
        cache,
        registry,
        Some(handle.clone()),
        watchers,
    );
    tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    (tmp, sock, handle)
}

#[tokio::test]
async fn refresh_git_provider() {
    let repo = create_test_repo();
    let (_tmp, sock, _handle) = setup_daemon().await;
    let client = Client::new(sock);

    let resp = client
        .refresh("git", Some(repo.path().to_str().unwrap()))
        .await
        .unwrap();
    assert!(resp.ok);

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let resp = client
        .get("git.branch", Some(repo.path().to_str().unwrap()))
        .await
        .unwrap();
    assert!(resp.ok, "Should get git branch");
    let branch = resp.data.unwrap();
    assert!(branch.is_string(), "Branch should be a string");
}

#[tokio::test]
async fn git_detects_dirty_state_via_refresh() {
    let repo = create_test_repo();
    let (_tmp, sock, _handle) = setup_daemon().await;
    let client = Client::new(sock);
    let repo_path = repo.path().to_str().unwrap();

    // First refresh: clean state
    client.refresh("git", Some(repo_path)).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let resp = client.get("git.dirty", Some(repo_path)).await.unwrap();
    assert_eq!(
        resp.data.unwrap(),
        serde_json::json!(false),
        "Should be clean"
    );

    // Create a dirty file
    std::fs::write(repo.path().join("dirty.txt"), "dirty").unwrap();

    // Refresh again
    client.refresh("git", Some(repo_path)).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let resp = client.get("git.dirty", Some(repo_path)).await.unwrap();
    assert_eq!(
        resp.data.unwrap(),
        serde_json::json!(true),
        "Should be dirty"
    );
}

#[tokio::test]
async fn git_returns_miss_for_non_repo() {
    let tmp = TempDir::new().unwrap();
    let (_daemon_tmp, sock, _handle) = setup_daemon().await;
    let client = Client::new(sock);

    client
        .refresh("git", Some(tmp.path().to_str().unwrap()))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let resp = client
        .get("git.branch", Some(tmp.path().to_str().unwrap()))
        .await
        .unwrap();
    assert!(resp.ok);
    assert!(resp.data.is_none(), "Non-git dir should return miss");
}
