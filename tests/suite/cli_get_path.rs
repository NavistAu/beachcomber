use crate::common::fetch::fetch;
use beachcomber::cache::Cache;
/// Integration tests for path context handling in `comb get`.
///
/// These tests exercise the directory a request addresses — explicit, or the
/// one an SDK session keeps and sends with every request
/// (`libbeachcomber::Session::set_context`) — to verify that path-scoped
/// queries work correctly, which is the server behaviour the CLI
/// disambiguation logic (split_keys_and_path) routes to.
///
/// Unit tests for the disambiguation logic itself live in src/main.rs
/// under `#[cfg(test)] mod path_disambiguation_tests`.
use beachcomber::cache::Schedule;
use beachcomber::provider::Value;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::server::Server;
use libbeachcomber::resolve::FetchError;
use libbeachcomber::{ClientConfig, Session};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

/// A `libbeachcomber` session on `sock`, with no daemon auto-start.
fn session_for(sock: std::path::PathBuf) -> Session {
    libbeachcomber::Client::with_config(ClientConfig {
        timeout: Duration::from_secs(2),
        auto_start: false,
        exec: false,
    })
    .with_socket_path(sock)
    .session()
    .unwrap()
}

/// The rendered value; a miss is a panic naming `what`.
fn hit(result: Option<serde_json::Value>, what: &str) -> serde_json::Value {
    result.unwrap_or_else(|| panic!("{what}: expected a value, got a miss"))
}

/// Set up a server with a real git repo on disk, so the git provider's
/// `root` (which walks up looking for `.git`) actually finds a
/// repo root and returns the expected path.
///
/// Returns:
/// - the tempdir holding the socket
/// - the socket path
/// - the repo dir (a separate tempdir with `.git` inside; passed to the server
///   via `set_context` / explicit path in queries)
async fn setup_server_with_git_repo() -> (TempDir, std::path::PathBuf, TempDir, String) {
    let sock_tmp = TempDir::new().unwrap();
    let sock = sock_tmp.path().join("test.sock");
    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(ProviderRegistry::with_defaults());

    // Create a real git repo the root walk can resolve to.
    let repo_tmp = TempDir::new().unwrap();
    let repo_path = repo_tmp
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();
    // Force the initial branch to `main` regardless of the host's
    // `init.defaultBranch` (CI runners default to `master`); a read checks the
    // record against HEAD, so the git provider reports the repo's real branch,
    // which these tests assert == "main".
    crate::common::git::git(std::path::Path::new(&repo_path), &["init", "-b", "main"]);

    // Seed a git.branch entry at the repo root (where root will land).
    let mut git = HashMap::new();
    git.insert("branch".to_string(), Value::String("main".to_string()));
    git.insert("dirty".to_string(), Value::Bool(false));
    cache.put_source("git", Some(&repo_path), "refs", git, Schedule::poll(60, 1));

    // Seed a hostname entry (global — no path).
    let mut hostname = HashMap::new();
    hostname.insert("name".to_string(), Value::String("myhost".to_string()));
    cache.put_source("hostname", None, "host", hostname, Schedule::poll(60, 1));

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    server.spawn().unwrap();

    (sock_tmp, sock, repo_tmp, repo_path)
}

/// Path context set via set_context reaches the correct cache entry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn path_context_resolves_git_branch() {
    let (_sock_tmp, sock, _repo_tmp, repo_path) = setup_server_with_git_repo().await;
    let mut session = session_for(sock);

    session.set_context(&repo_path).unwrap();
    let branch = hit(session.get("git.branch", None).unwrap(), "git.branch");

    assert_eq!(branch, "main");
}

/// A session's directory is sent with every request that names none — `put`
/// and `refresh` as well as `get` — and an explicit path still wins.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sessions_directory_reaches_put_as_well_as_get() {
    let (_sock_tmp, sock, _repo_tmp, repo_path) = setup_server_with_git_repo().await;
    let mut session = session_for(sock.clone());
    session.set_context(&repo_path).unwrap();

    session
        .put("sessdir", serde_json::json!({"v": "here"}), None, None)
        .unwrap();
    assert_eq!(
        hit(session.get("sessdir.v", None).unwrap(), "sessdir.v"),
        "here"
    );

    let at_repo = fetch(&sock, "sessdir.v", Some(&repo_path))
        .expect("an answer")
        .expect("the put landed at the session's directory");
    assert_eq!(at_repo.data.as_ref().unwrap(), "here");
    let global = fetch(&sock, "sessdir.v", None).expect("an answer");
    assert!(global.is_none(), "nothing was put globally: {global:?}");
}

/// Global providers (hostname) work regardless of path context.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn global_provider_ignores_path_context() {
    let (_sock_tmp, sock, _repo_tmp, _repo_path) = setup_server_with_git_repo().await;
    let mut session = session_for(sock);

    // Set context to a path where no git data exists.
    session.set_context("/some/other/path").unwrap();
    let name = hit(session.get("hostname.name", None).unwrap(), "hostname.name");

    assert_eq!(name, "myhost");
}

/// Querying multiple keys in one session shares the same path context.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multiple_keys_share_path_context() {
    let (_sock_tmp, sock, _repo_tmp, repo_path) = setup_server_with_git_repo().await;
    let mut session = session_for(sock);

    session.set_context(&repo_path).unwrap();

    let branch = hit(session.get("git.branch", None).unwrap(), "git.branch");
    let dirty = hit(session.get("git.dirty", None).unwrap(), "git.dirty");

    assert_eq!(branch, "main");
    assert_eq!(dirty, false);
}

/// Without set_context, a path-scoped key is still queried (may return not-found).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_context_path_scoped_query_returns_response() {
    let (_sock_tmp, sock, _repo_tmp, _repo_path) = setup_server_with_git_repo().await;
    // Use get() without set_context — server receives no path.
    let resp = fetch(&sock, "git.branch", None);

    // We just verify the server responds (hit, miss or rejection), not that it panics.
    assert!(!matches!(resp, Err(FetchError::Transport(_))), "{resp:?}");
}

/// Passing explicit path to get() (not via set_context) also works.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_path_in_get_resolves_correctly() {
    let (_sock_tmp, sock, _repo_tmp, repo_path) = setup_server_with_git_repo().await;
    // Pass path directly to get() rather than using set_context.
    let resp = fetch(&sock, "git.branch", Some(&repo_path))
        .expect("expected ok response with explicit path")
        .expect("expected a hit with explicit path");
    assert_eq!(resp.data.as_ref().unwrap(), "main");
}

/// Canonical-path dedup: querying git from a subdir of the repo resolves to
/// the repo root, hitting the same cache entry seeded at the root.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subdir_query_resolves_to_repo_root() {
    let (_sock_tmp, sock, _repo_tmp, repo_path) = setup_server_with_git_repo().await;

    // Create a subdir inside the repo and query git.branch from there.
    let subdir = std::path::Path::new(&repo_path).join("src").join("lib");
    std::fs::create_dir_all(&subdir).unwrap();

    let resp = fetch(&sock, "git.branch", Some(subdir.to_str().unwrap()))
        .expect("expected ok — subdir should canonicalise to repo root")
        .expect("expected a hit — subdir should canonicalise to repo root");

    // Cache was seeded only at the repo root. If root didn't walk
    // up, this lookup would miss. Success proves subdir→root resolution.
    assert_eq!(resp.data.as_ref().unwrap(), "main");
}
