//! Key addressing: which forms of a key resolve to a value and which miss.
//!
//! `provider.source` and `provider.source.field` key addressing has been
//! removed: a source name is metadata on the record, never part of an
//! address (`docs/canon/provider_source.md` "Record"). `parse_key` in
//! `src/query.rs` no longer disambiguates a second segment against the
//! registry — it is always the field. These tests pin the resulting wire
//! behaviour through a real socket, mirroring `tests/suite/store.rs`/
//! `tests/suite/facets.rs`'s raw-socket style, against a real git repo (`git` has
//! a `refs` source with an `ahead` field, so `git.refs`/`git.refs.ahead` is
//! exactly the collision this change removes). This is an area file for
//! key-addressing behaviour generally — later key-addressing tests join it
//! here rather than starting a new file.

use crate::common::git::GitRepoFixture;
use beachcomber::cache::Cache;
use beachcomber::protocol::Response;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::server::Server;
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

/// The documented shape of a miss is `{"ok":true}` with every other field
/// absent — not just an absent `data`.
fn assert_miss(resp: &Response, context: &str) {
    assert!(
        resp.ok,
        "{context}: a miss is still ok, got error {:?}",
        resp.error
    );
    assert!(
        resp.data.is_none(),
        "{context}: data should be absent, got {:?}",
        resp.data
    );
    assert!(
        resp.age_ms.is_none(),
        "{context}: age_ms should be absent, got {:?}",
        resp.age_ms
    );
    assert!(
        resp.stale.is_none(),
        "{context}: stale should be absent, got {:?}",
        resp.stale
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_name_as_key_segment_misses_but_the_field_still_hits() {
    if !crate::common::git::has_git() {
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
    assert_miss(&resp, "git.refs before warming");

    // "ahead" is a real field of the "refs" source, but addressing it through
    // the (removed) source-field form also just misses now.
    let resp = send_recv(
        &mut stream,
        &serde_json::json!({"op": "get", "key": "git.refs.ahead", "path": path}),
    )
    .await;
    assert_miss(&resp, "git.refs.ahead before warming");

    // git.ahead is the field's one real address, and it still hits — this
    // also warms the "refs" source in the cache.
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

    // A source name is not a key even once the source is warm: "refs" is
    // still not a field, regardless of whether its data now sits in cache.
    let resp = send_recv(
        &mut stream,
        &serde_json::json!({"op": "get", "key": "git.refs", "path": path}),
    )
    .await;
    assert_miss(&resp, "git.refs after warming refs");

    let resp = send_recv(
        &mut stream,
        &serde_json::json!({"op": "get", "key": "git.refs.ahead", "path": path}),
    )
    .await;
    assert_miss(&resp, "git.refs.ahead after warming refs");

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn whole_provider_flattening_has_no_key_named_for_a_source() {
    if !crate::common::git::has_git() {
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
    if !crate::common::git::has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let repo = GitRepoFixture::new();
    let path = repo.path_str();

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let request = serde_json::json!({"op": "watch", "key": "git.refs", "path": path}).to_string();
    writer
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut line = String::new();
    let result = tokio::time::timeout(Duration::from_millis(500), reader.read_line(&mut line))
        .await
        .expect("timed out waiting for the initial watch frame")
        .unwrap();
    assert!(result > 0, "expected a response line");

    let resp: Response = serde_json::from_str(&line).unwrap();
    assert_miss(&resp, "watch git.refs initial frame");

    handle.abort();
}
