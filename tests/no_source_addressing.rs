//! `provider.source` and `provider.source.field` key addressing has been
//! removed: a source name is metadata on a record, never part of an address
//! (`docs/canon/provider_source.md` "Record"). `parse_key` in `src/query.rs`
//! no longer disambiguates a second segment against the registry — it is
//! always the field. These tests pin the resulting wire behaviour through a
//! real socket, mirroring `tests/store.rs`/`tests/facets.rs`'s raw-socket
//! style, against a real git repo (`git` has a `refs` source with an `ahead`
//! field, so `git.refs`/`git.refs.ahead` is exactly the collision this
//! change removes).

mod common;

use beachcomber::cache::Cache;
use beachcomber::protocol::Response;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::server::Server;
use common::git::GitRepoFixture;
use std::sync::Arc;
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
    let registry = Arc::new(ProviderRegistry::with_defaults());
    (tmp, sock, cache, registry, watchers)
}

async fn send_recv(stream: &mut UnixStream, request: &serde_json::Value) -> Response {
    let mut line = request.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes()).await.unwrap();
    let mut reader = BufReader::new(&mut *stream);
    let mut resp_line = String::new();
    reader.read_line(&mut resp_line).await.unwrap();
    serde_json::from_str(&resp_line).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_name_as_key_segment_misses_but_the_field_still_hits() {
    if !common::git::has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let repo = GitRepoFixture::new();
    let path = repo.path_str();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    // "refs" is a source name, not a field — git.refs is a miss, not an
    // error: an unknown field is a miss like any other.
    let resp = send_recv(
        &mut stream,
        &serde_json::json!({"op": "get", "key": "git.refs", "path": path}),
    )
    .await;
    assert!(
        resp.ok,
        "unknown field is a miss, not an error: {:?}",
        resp.error
    );
    assert!(
        resp.data.is_none(),
        "git.refs must miss, got {:?}",
        resp.data
    );

    // "ahead" is a real field of the "refs" source, but addressing it through
    // the (removed) source-field form also just misses now.
    let resp = send_recv(
        &mut stream,
        &serde_json::json!({"op": "get", "key": "git.refs.ahead", "path": path}),
    )
    .await;
    assert!(resp.ok, "unexpected error: {:?}", resp.error);
    assert!(
        resp.data.is_none(),
        "git.refs.ahead must miss, got {:?}",
        resp.data
    );

    // git.ahead is the field's one real address, and it still hits.
    let resp = send_recv(
        &mut stream,
        &serde_json::json!({"op": "get", "key": "git.ahead", "path": path}),
    )
    .await;
    assert!(resp.ok, "git.ahead should hit: {:?}", resp.error);
    assert!(
        resp.data.as_ref().is_some_and(|d| d.is_number()),
        "git.ahead should be numeric, got {:?}",
        resp.data
    );

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn whole_provider_flattening_has_no_key_named_for_a_source() {
    if !common::git::has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let repo = GitRepoFixture::new();
    let path = repo.path_str();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    // Warm the "refs" source first (git's `head` source is `read_always`, so
    // a bare `git` cold-miss stops at the first source it re-executes and
    // never reaches the rest — a pre-existing gap in whole-provider cold-miss
    // unrelated to this task; querying the field first sidesteps it).
    let resp = send_recv(
        &mut stream,
        &serde_json::json!({"op": "get", "key": "git.ahead", "path": path}),
    )
    .await;
    assert!(resp.ok, "git.ahead should hit: {:?}", resp.error);

    let resp = send_recv(
        &mut stream,
        &serde_json::json!({"op": "get", "key": "git", "path": path}),
    )
    .await;
    assert!(resp.ok, "whole-provider get should hit: {:?}", resp.error);
    let data = resp.data.expect("git provider should have data");
    let obj = data.as_object().expect("object");
    assert!(
        obj.contains_key("ahead"),
        "flattened fields include ahead, got keys: {:?}",
        obj.keys().collect::<Vec<_>>()
    );
    assert!(
        !obj.contains_key("refs"),
        "a source name is never a key in the flattened object, got keys: {:?}",
        obj.keys().collect::<Vec<_>>()
    );

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watch_on_a_source_name_key_opens_and_misses() {
    if !common::git::has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let repo = GitRepoFixture::new();
    let path = repo.path_str();

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    let resp = send_recv(
        &mut stream,
        &serde_json::json!({"op": "watch", "key": "git.refs", "path": path}),
    )
    .await;
    assert!(
        resp.ok,
        "watch on an unknown field is accepted, not an error: {:?}",
        resp.error
    );
    assert!(
        resp.data.is_none(),
        "watch's initial frame for an unknown field is a miss, got {:?}",
        resp.data
    );

    handle.abort();
}
