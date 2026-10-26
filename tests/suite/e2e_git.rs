use crate::common::fetch::fetch;
use crate::common::git::GitRepoFixture;
use crate::common::poll::{poll_until, poll_until_async, poll_until_eq};
use beachcomber::cache::Cache;
use beachcomber::client::Client;
use beachcomber::config::Config;
use beachcomber::scheduler::Scheduler;
use beachcomber::server::Server;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

async fn setup_daemon() -> (
    TempDir,
    std::path::PathBuf,
    beachcomber::scheduler::SchedulerHandle,
) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(crate::common::registry::with_shipped_defaults());
    let config = Config::default();

    let (handle, mut scheduler) =
        Scheduler::new(cache.clone(), registry.clone(), config, watchers.clone());
    // These tests are about git data, not the watch backend: a kernel FSEvents
    // registration costs ~1.2s per watch() on macOS and the runtime drop waits
    // for it. `SchedulerHandle::send` buffers until `run()` receives, so there
    // is nothing to wait for after spawning.
    scheduler.use_polling_watcher(Duration::from_millis(50));
    tokio::spawn(async move { scheduler.run().await });

    let server = Server::new(
        sock.clone(),
        cache,
        registry,
        Some(handle.clone()),
        watchers,
    );
    // `spawn` binds the socket synchronously — it is connectable on return.
    server.spawn().expect("server binds socket");

    (tmp, sock, handle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_git_provider() {
    let repo = GitRepoFixture::new();
    let (_tmp, sock, _handle) = setup_daemon().await;
    let client = Client::new(sock.clone());

    let resp = client.refresh("git", Some(repo.path_str())).unwrap();
    assert!(resp.ok);

    let data = poll_until(
        "git.branch after refresh",
        "a get response carrying a string branch",
        Duration::from_secs(5),
        Duration::from_millis(20),
        || {
            fetch(&sock, "git.branch", Some(repo.path_str()))
                .unwrap()
                .and_then(|got| got.data)
        },
        |d| d.as_ref().is_some_and(|d| d.is_string()),
    );
    let branch = data.expect("Should get git branch");
    assert!(branch.is_string(), "Branch should be a string");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_detects_dirty_state_via_refresh() {
    let repo = GitRepoFixture::new();
    let (_tmp, sock, _handle) = setup_daemon().await;
    let client = Client::new(sock.clone());
    let repo_path = repo.path_str();

    // First refresh: clean state
    client.refresh("git", Some(repo_path)).unwrap();
    poll_until_eq(
        "git.dirty after refresh (clean repo)",
        Duration::from_secs(5),
        Duration::from_millis(20),
        || {
            fetch(&sock, "git.dirty", Some(repo_path))
                .unwrap()
                .and_then(|got| got.data)
        },
        Some(serde_json::json!(false)),
    );

    // Create a dirty file
    std::fs::write(repo.path().join("dirty.txt"), "dirty").unwrap();

    // Refresh again
    client.refresh("git", Some(repo_path)).unwrap();
    poll_until_eq(
        "git.dirty after refresh (dirty repo)",
        Duration::from_secs(5),
        Duration::from_millis(20),
        || {
            fetch(&sock, "git.dirty", Some(repo_path))
                .unwrap()
                .and_then(|got| got.data)
        },
        Some(serde_json::json!(true)),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_returns_miss_for_non_repo() {
    let tmp = TempDir::new().unwrap();
    let (_daemon_tmp, sock, handle) = setup_daemon().await;
    let client = Client::new(sock.clone());

    client
        .refresh("git", Some(tmp.path().to_str().unwrap()))
        .unwrap();

    // The refresh is asynchronous: the scheduler fans it out to every git
    // source. The scheduler handles its messages in order, so by the time it
    // answers a status request the refresh has started, and the refresh is done
    // once nothing is in flight. A non-repo path makes every source report
    // nothing, which is a result and not a failure.
    poll_until_async(
        "the scheduler's in-flight sources after refreshing git on a non-repo",
        "none in flight, i.e. the refresh finished",
        Duration::from_secs(5),
        Duration::from_millis(20),
        async || handle.get_status().await.expect("scheduler runs").in_flight,
        |in_flight| in_flight.is_empty(),
    )
    .await;
    assert!(
        handle.get_failure_states().await.is_empty(),
        "a directory with nothing to report is not a failing source"
    );

    let data = fetch(&sock, "git.branch", Some(tmp.path().to_str().unwrap()))
        .unwrap()
        .and_then(|got| got.data);
    assert!(data.is_none(), "Non-git dir should return miss");
}
