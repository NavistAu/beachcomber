/// Integration tests for path context handling in `comb get`.
///
/// These tests exercise the server-side path context (set via `ClientSession::set_context`)
/// to verify that path-scoped queries work correctly — which is the server behaviour that
/// the CLI disambiguation logic (split_keys_and_path) routes to.
///
/// Unit tests for the disambiguation logic itself live in src/main.rs
/// under `#[cfg(test)] mod path_disambiguation_tests`.
use beachcomber::cache::Cache;
use beachcomber::client::Client;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{ProviderResult, Value};
use beachcomber::server::Server;
use std::sync::Arc;
use tempfile::TempDir;

async fn setup_server_with_git(git_path: &str) -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(ProviderRegistry::with_defaults());

    // Seed a git.branch entry for the given path.
    let mut git = ProviderResult::new();
    git.insert("branch", Value::String("main".to_string()));
    git.insert("dirty", Value::Bool(false));
    cache.put("git", Some(git_path), git);

    // Seed a hostname entry (global — no path).
    let mut hostname = ProviderResult::new();
    hostname.insert("name", Value::String("myhost".to_string()));
    cache.put("hostname", None, hostname);

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    (tmp, sock)
}

/// Path context set via set_context reaches the correct cache entry.
#[tokio::test]
async fn path_context_resolves_git_branch() {
    let path = "/tmp/myrepo";
    let (_tmp, sock) = setup_server_with_git(path).await;
    let client = Client::new(sock);
    let mut session = client.connect().await.unwrap();

    session.set_context(path).await.unwrap();
    let resp = session.get("git.branch", None).await.unwrap();

    assert!(resp.ok, "expected ok response, got: {:?}", resp.error);
    assert_eq!(resp.data.as_ref().unwrap(), "main");
}

/// Global providers (hostname) work regardless of path context.
#[tokio::test]
async fn global_provider_ignores_path_context() {
    let path = "/tmp/irrelevant";
    let (_tmp, sock) = setup_server_with_git(path).await;
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
    let path = "/tmp/shared-context-repo";
    let (_tmp, sock) = setup_server_with_git(path).await;
    let client = Client::new(sock);
    let mut session = client.connect().await.unwrap();

    session.set_context(path).await.unwrap();

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
    let (_tmp, sock) = setup_server_with_git("/tmp/no-context-repo").await;
    let client = Client::new(sock);

    // Use get() without set_context — server receives no path.
    // The cached entry is at "/tmp/no-context-repo"; without that path, it won't match.
    let resp = client.get("git.branch", None).await.unwrap();

    // We just verify the server responds (ok or not-ok), not that it panics.
    // The response should be a protocol response either way.
    let _ = resp.ok; // either result is valid — just checking no panic/error
}

/// Passing explicit path to get() (not via set_context) also works.
#[tokio::test]
async fn explicit_path_in_get_resolves_correctly() {
    let path = "/tmp/explicit-path-repo";
    let (_tmp, sock) = setup_server_with_git(path).await;
    let client = Client::new(sock);

    // Pass path directly to get() rather than using set_context.
    let resp = client.get("git.branch", Some(path)).await.unwrap();

    assert!(
        resp.ok,
        "expected ok response with explicit path: {:?}",
        resp.error
    );
    assert_eq!(resp.data.as_ref().unwrap(), "main");
}
