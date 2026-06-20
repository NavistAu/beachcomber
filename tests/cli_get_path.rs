/// Integration tests for path context handling in `comb get`.
///
/// These tests exercise the server-side path context (set via `ClientSession::set_context`)
/// to verify that path-scoped queries work correctly — which is the server behaviour that
/// the CLI disambiguation logic (split_keys_and_path) routes to.
///
/// Unit tests for the disambiguation logic itself live in src/main.rs
/// under `#[cfg(test)] mod path_disambiguation_tests`.
mod common;

use beachcomber::cache::Cache;
use beachcomber::client::Client;
use beachcomber::provider::Value;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::server::Server;
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;

/// Set up a server with a real git repo on disk, so the git provider's
/// `canonical_path` (which walks up looking for `.git`) actually finds a
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

    // Create a real git repo the canonical_path walk can resolve to.
    let repo_tmp = TempDir::new().unwrap();
    let repo_path = repo_tmp
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();
    // Force the initial branch to `main` regardless of the host's
    // `init.defaultBranch` (CI runners default to `master`); the read-always git
    // provider reports the repo's real branch, which these tests assert == "main".
    std::process::Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(&repo_path)
        .output()
        .unwrap();

    // Seed a git.branch entry at the repo root (where canonical_path will land).
    let mut git = HashMap::new();
    git.insert("branch".to_string(), Value::String("main".to_string()));
    git.insert("dirty".to_string(), Value::Bool(false));
    cache.put_source("git", Some(&repo_path), "refs", git, Some(60));

    // Seed a hostname entry (global — no path).
    let mut hostname = HashMap::new();
    hostname.insert("name".to_string(), Value::String("myhost".to_string()));
    cache.put_source("hostname", None, "main", hostname, Some(60));

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    (sock_tmp, sock, repo_tmp, repo_path)
}

/// Path context set via set_context reaches the correct cache entry.
#[tokio::test]
async fn path_context_resolves_git_branch() {
    if !common::git::has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let (_sock_tmp, sock, _repo_tmp, repo_path) = setup_server_with_git_repo().await;
    let client = Client::new(sock);
    let mut session = client.connect().await.unwrap();

    session.set_context(&repo_path).await.unwrap();
    let resp = session.get("git.branch", None).await.unwrap();

    assert!(resp.ok, "expected ok response, got: {:?}", resp.error);
    assert_eq!(resp.data.as_ref().unwrap(), "main");
}

/// Global providers (hostname) work regardless of path context.
#[tokio::test]
async fn global_provider_ignores_path_context() {
    if !common::git::has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let (_sock_tmp, sock, _repo_tmp, _repo_path) = setup_server_with_git_repo().await;
    let client = Client::new(sock);
    let mut session = client.connect().await.unwrap();

    // Set context to a path where no git data exists.
    session.set_context("/some/other/path").await.unwrap();
    let resp = session.get("hostname.name", None).await.unwrap();

    assert!(resp.ok, "expected ok for global provider: {:?}", resp.error);
    assert_eq!(resp.data.as_ref().unwrap(), "myhost");
}

/// Querying multiple keys in one session shares the same path context.
#[tokio::test]
async fn multiple_keys_share_path_context() {
    if !common::git::has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let (_sock_tmp, sock, _repo_tmp, repo_path) = setup_server_with_git_repo().await;
    let client = Client::new(sock);
    let mut session = client.connect().await.unwrap();

    session.set_context(&repo_path).await.unwrap();

    let branch_resp = session.get("git.branch", None).await.unwrap();
    let dirty_resp = session.get("git.dirty", None).await.unwrap();

    assert!(branch_resp.ok);
    assert_eq!(branch_resp.data.as_ref().unwrap(), "main");

    assert!(dirty_resp.ok);
    assert_eq!(dirty_resp.data.as_ref().unwrap(), false);
}

/// Without set_context, a path-scoped key is still queried (may return not-found).
#[tokio::test]
async fn no_context_path_scoped_query_returns_response() {
    if !common::git::has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let (_sock_tmp, sock, _repo_tmp, _repo_path) = setup_server_with_git_repo().await;
    let client = Client::new(sock);

    // Use get() without set_context — server receives no path.
    let resp = client.get("git.branch", None).await.unwrap();

    // We just verify the server responds (ok or not-ok), not that it panics.
    let _ = resp.ok;
}

/// Passing explicit path to get() (not via set_context) also works.
#[tokio::test]
async fn explicit_path_in_get_resolves_correctly() {
    if !common::git::has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let (_sock_tmp, sock, _repo_tmp, repo_path) = setup_server_with_git_repo().await;
    let client = Client::new(sock);

    // Pass path directly to get() rather than using set_context.
    let resp = client.get("git.branch", Some(&repo_path)).await.unwrap();

    assert!(
        resp.ok,
        "expected ok response with explicit path: {:?}",
        resp.error
    );
    assert_eq!(resp.data.as_ref().unwrap(), "main");
}

/// Canonical-path dedup: querying git from a subdir of the repo resolves to
/// the repo root, hitting the same cache entry seeded at the root.
#[tokio::test]
async fn subdir_query_resolves_to_repo_root() {
    if !common::git::has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let (_sock_tmp, sock, _repo_tmp, repo_path) = setup_server_with_git_repo().await;

    // Create a subdir inside the repo and query git.branch from there.
    let subdir = std::path::Path::new(&repo_path).join("src").join("lib");
    std::fs::create_dir_all(&subdir).unwrap();

    let client = Client::new(sock);
    let resp = client
        .get("git.branch", Some(subdir.to_str().unwrap()))
        .await
        .unwrap();

    // Cache was seeded only at the repo root. If canonical_path didn't walk
    // up, this lookup would miss. Success proves subdir→root resolution.
    assert!(
        resp.ok,
        "expected ok — subdir should canonicalise to repo root: {:?}",
        resp.error
    );
    assert_eq!(resp.data.as_ref().unwrap(), "main");
}
