//! `refresh` re-executes the Sources a key demands — each at its own scope —
//! and refuses a key no Source answers (spec §3.7).
//!
//! Most servers here are built without a scheduler, so a refresh runs inline on
//! the connection task and the response is written only once the Sources have
//! run. That is what lets those cases advance the clock by a known amount and
//! assert exact ages. The scheduler-backed case at the bottom runs the refresh
//! in the background, so it waits for the age of `git.commit` to fall back
//! below what the advanced clock made it.
//!
//! `git` is the fixture because it is the one shipped provider with several Sources
//! over one namespace. The fields under test are `commit` (source `refs`) and
//! `lines_added` (source `diff`).

use crate::common::git::GitRepoFixture;
use crate::common::poll::poll_until_async;
use beachcomber::cache::Cache;
use beachcomber::protocol::Response;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::server::Server;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

fn setup() -> (
    TempDir,
    std::path::PathBuf,
    Arc<Cache>,
    Arc<ProviderRegistry>,
    Arc<beachcomber::watcher_registry::WatcherRegistry>,
) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(crate::common::registry::with_shipped_defaults());
    (tmp, sock, cache, registry, watchers)
}

async fn send_recv(stream: &mut UnixStream, request: &serde_json::Value) -> Response {
    let mut line = request.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes()).await.unwrap();
    let mut reader = BufReader::new(&mut *stream);
    let mut resp = String::new();
    reader.read_line(&mut resp).await.unwrap();
    serde_json::from_str(&resp).unwrap()
}

fn get(key: &str, path: &str) -> serde_json::Value {
    json!({"op": "get", "key": key, "path": path})
}

fn refresh(key: &str, path: &str) -> serde_json::Value {
    json!({"op": "refresh", "key": key, "path": path})
}

/// The `data` of a warm `get`, asserted present.
async fn warm(stream: &mut UnixStream, key: &str, path: &str) -> serde_json::Value {
    let resp = send_recv(stream, &get(key, path)).await;
    assert!(resp.ok, "get {key}: {:?}", resp.error);
    resp.data
        .unwrap_or_else(|| panic!("get {key} should hit at {path}"))
}

/// The `age_ms` of a key's `:cache` leaf (`git.commit` -> `git:cache.commit`),
/// in ms. A `:cache` read runs nothing.
async fn age(stream: &mut UnixStream, key: &str, path: &str) -> u64 {
    let cache_key = key.replacen('.', ":cache.", 1);
    let resp = send_recv(stream, &get(&cache_key, path)).await;
    assert!(resp.ok, "get {cache_key}: {:?}", resp.error);
    resp.data
        .as_ref()
        .and_then(|d| d["age_ms"].as_u64())
        .unwrap_or_else(|| panic!("{cache_key} should carry an age_ms, got {:?}", resp.data))
}

/// Age every record in the cache by 10 s. The cache stamps records with
/// `tokio::time::Instant`, so the offset survives `resume`. The margin is
/// what the "re-executed" assertions below lean on: a re-executed record's
/// age is the real time between its write and the next `:cache` read, and
/// under a loaded suite that gap can pass 100 ms. Ten seconds is below every
/// git source's poll interval, so nothing turns stale, and a scheduler on the
/// same clock polls nothing.
async fn advance_10s() {
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(10)).await;
    tokio::time::resume();
}

#[tokio::test]
async fn refreshing_a_field_re_executes_only_its_owning_source() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let repo = GitRepoFixture::new();
    let path = repo.path_str();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    warm(&mut s, "git.commit", path).await;
    warm(&mut s, "git.lines_added", path).await;
    advance_10s().await;

    let refs_before = age(&mut s, "git.commit", path).await;
    let diff_before = age(&mut s, "git.lines_added", path).await;
    assert!(
        refs_before >= 10_000 && diff_before >= 10_000,
        "both records should carry the advanced clock, got refs={refs_before} diff={diff_before}"
    );

    let resp = send_recv(&mut s, &refresh("git.commit", path)).await;
    assert!(resp.ok, "refresh git.commit: {:?}", resp.error);
    assert!(
        age(&mut s, "git.commit", path).await < refs_before,
        "refresh git.commit must re-execute `refs`"
    );
    assert!(
        age(&mut s, "git.lines_added", path).await >= diff_before,
        "refresh git.commit must leave `diff` alone"
    );

    // The same rule for `git.branch`: `head` is the only source it demands,
    // so `diff` is still untouched.
    let resp = send_recv(&mut s, &refresh("git.branch", path)).await;
    assert!(resp.ok, "refresh git.branch: {:?}", resp.error);
    assert!(
        age(&mut s, "git.lines_added", path).await >= diff_before,
        "refresh git.branch must leave `diff` alone"
    );

    handle.abort();
}

#[tokio::test]
async fn refreshing_a_namespace_re_executes_every_source() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let repo = GitRepoFixture::new();
    let path = repo.path_str();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    warm(&mut s, "git.commit", path).await;
    warm(&mut s, "git.lines_added", path).await;
    advance_10s().await;

    let refs_before = age(&mut s, "git.commit", path).await;
    let diff_before = age(&mut s, "git.lines_added", path).await;

    let resp = send_recv(&mut s, &refresh("git", path)).await;
    assert!(resp.ok, "refresh git: {:?}", resp.error);
    assert!(
        age(&mut s, "git.commit", path).await < refs_before,
        "a namespace refresh re-executes `refs`"
    );
    assert!(
        age(&mut s, "git.lines_added", path).await < diff_before,
        "a namespace refresh re-executes `diff`"
    );

    handle.abort();
}

/// git is nearest: in a directory with no repository it has no instance, so a
/// refresh there has nothing to re-execute, and says so instead of claiming ok.
#[tokio::test]
async fn a_nearest_provider_with_no_instance_here_cannot_be_refreshed() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();
    let outside = TempDir::new().unwrap();
    let outside = outside.path().canonicalize().unwrap();
    let outside = outside.to_str().unwrap();

    for key in ["git", "git.branch"] {
        let resp = send_recv(&mut s, &refresh(key, outside)).await;
        assert!(!resp.ok, "refresh {key} should error");
        assert_eq!(
            resp.error,
            Some(format!(
                "nothing to refresh: {key} has no instance at {outside}"
            ))
        );
    }

    handle.abort();
}

#[tokio::test]
async fn a_key_no_source_answers_cannot_be_refreshed() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    // `refs` is a source name, and a source name is never a field: no source
    // owns it, so there is nothing for a refresh to re-execute.
    let resp = send_recv(&mut s, &json!({"op": "refresh", "key": "git.refs"})).await;
    assert!(!resp.ok, "refresh git.refs should error");
    assert_eq!(
        resp.error.as_deref(),
        Some("nothing to refresh: git.refs has no sources")
    );

    // An unknown namespace has no sources either — and says so, rather than
    // borrowing `get`'s "unknown provider".
    let resp = send_recv(&mut s, &json!({"op": "refresh", "key": "nope"})).await;
    assert!(!resp.ok, "refresh nope should error");
    assert_eq!(
        resp.error.as_deref(),
        Some("nothing to refresh: nope has no sources")
    );

    handle.abort();
}

#[tokio::test]
async fn a_global_refresh_without_a_home_repository_has_nothing_to_refresh() {
    // SAFETY: nextest gives every test its own process, so no other thread is
    // reading the environment concurrently. `$HOME` is the global path, and it
    // has to be somewhere that is not a repository for this to mean anything.
    let home = TempDir::new().unwrap();
    unsafe { std::env::set_var("HOME", home.path().canonicalize().unwrap()) };

    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    // git declares sources, but it is nearest and `$HOME` is no work tree: it
    // has no instance there, so there is nothing to refresh.
    let resp = send_recv(&mut s, &json!({"op": "refresh", "key": "git"})).await;
    assert!(!resp.ok, "refresh git --global should error");
    assert_eq!(
        resp.error.as_deref(),
        Some("nothing to refresh: git has no instance at ~")
    );

    let resp = send_recv(&mut s, &json!({"op": "get", "key": "git.branch"})).await;
    assert!(resp.ok, "get git.branch --global: {:?}", resp.error);
    assert!(
        resp.data.is_none(),
        "a non-repository $HOME answers nothing, got {:?}",
        resp.data
    );

    handle.abort();
}

/// The scheduler runs a refresh in the background: both records are aged by
/// the clock — nothing writes to the repository meanwhile, so no file watch
/// can refresh either source behind our back — and a re-executed record is
/// then the one whose age has fallen back below that.
#[tokio::test]
async fn a_scheduler_backed_refresh_runs_the_demanded_source_alone() {
    // SAFETY: nextest gives every test its own process, so no other thread is
    // reading the environment concurrently. A real scheduler runs behind this
    // daemon and `$HOME` is the global path it polls, so it must not be the
    // developer's own.
    let home = TempDir::new().unwrap();
    unsafe { std::env::set_var("HOME", home.path().canonicalize().unwrap()) };

    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let (handle, ready) = beachcomber::daemon::start_in_process_polling(
        sock.clone(),
        crate::common::registry::config_over_shipped(vec![]),
    );
    ready.await.expect("daemon ready");

    let repo = GitRepoFixture::new();
    let path = repo.path_str();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    warm(&mut s, "git.lines_added", path).await;
    warm(&mut s, "git.commit", path).await;
    advance_10s().await;

    let refs_before = age(&mut s, "git.commit", path).await;
    let diff_before = age(&mut s, "git.lines_added", path).await;
    assert!(
        refs_before >= 10_000 && diff_before >= 10_000,
        "both records should carry the advanced clock, got refs={refs_before} diff={diff_before}"
    );

    let resp = send_recv(&mut s, &refresh("git.commit", path)).await;
    assert!(resp.ok, "refresh git.commit: {:?}", resp.error);

    poll_until_async(
        "git:cache.commit age_ms after refresh git.commit",
        "back below its pre-refresh age",
        Duration::from_secs(5),
        Duration::from_millis(5),
        async || age(&mut s, "git.commit", path).await,
        |a| *a < refs_before,
    )
    .await;
    assert!(
        age(&mut s, "git.lines_added", path).await >= diff_before,
        "refresh git.commit demands `refs` alone: `diff` must not have run"
    );

    handle.abort();
}
