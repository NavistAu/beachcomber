//! The `:cache` facet: `p:cache.<path>` answers what is known about the record
//! a read of `p.<path>` resolves to — its derived keys (`age_ms`, `stale`,
//! `source`) and the stored ones (`poll`, `watch`, `decay`). Like every read, a
//! get of a node returns that part of the tree: an object node answers a tree
//! shaped like its value, with the leaf answer at each leaf.

use crate::common::git::{GitRepoFixture, has_git};
use beachcomber::cache::Cache;
use beachcomber::protocol::Response;
use beachcomber::provider::Value;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::server::Server;
use beachcomber::watcher_registry::WatcherRegistry;
use serde_json::json;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

fn setup() -> (
    TempDir,
    std::path::PathBuf,
    Arc<Cache>,
    Arc<ProviderRegistry>,
    Arc<WatcherRegistry>,
) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let watchers = Arc::new(WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(ProviderRegistry::with_defaults());
    (tmp, sock, cache, registry, watchers)
}

async fn send(stream: &mut UnixStream, request: serde_json::Value) -> Response {
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    let mut reader = BufReader::new(&mut *stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    serde_json::from_str(&line).unwrap()
}

async fn get(stream: &mut UnixStream, key: &str, path: Option<&str>) -> Response {
    send(stream, json!({"op":"get","key":key,"path":path})).await
}

async fn put(
    stream: &mut UnixStream,
    key: &str,
    data: serde_json::Value,
    ttl: Option<&str>,
    path: Option<&str>,
) -> Response {
    send(
        stream,
        json!({"op":"put","key":key,"data":data,"ttl":ttl,"path":path}),
    )
    .await
}

async fn serve() -> (
    TempDir,
    UnixStream,
    Arc<Cache>,
    tokio::task::JoinHandle<std::io::Result<()>>,
    std::path::PathBuf,
) {
    let (tmp, sock, cache, registry, watchers) = setup();
    let handle = Server::new(sock.clone(), cache.clone(), registry, None, watchers)
        .spawn()
        .unwrap();
    let stream = UnixStream::connect(&sock).await.unwrap();
    (tmp, stream, cache, handle, sock)
}

#[tokio::test]
async fn the_cache_facet_of_a_source_leaf_names_the_source_and_its_schedule() {
    if !has_git() {
        return;
    }
    let (_tmp, mut s, _cache, handle, _sock) = serve().await;
    let repo = GitRepoFixture::new();
    let p = repo.path_str();

    // A plain get runs the Source (cold miss) and caches its record.
    assert_eq!(
        get(&mut s, "git.branch", Some(p)).await.data.unwrap(),
        "main"
    );

    let r = get(&mut s, "git:cache.branch", Some(p)).await;
    assert!(r.ok, "{:?}", r.error);
    let leaf = r.data.unwrap();
    assert!(leaf["age_ms"].is_u64(), "{leaf}");
    assert_eq!(leaf["stale"], json!(false));
    assert_eq!(leaf["source"], json!("source(head)"));
    // head is a Watch Source on `.git`; an array is an index-keyed object.
    assert_eq!(leaf["watch"], json!({"0": ".git"}), "{leaf}");
    assert!(leaf.get("poll").is_none(), "head does not poll: {leaf}");
    // An in-process daemon loads no managed defaults: the lifecycle default.
    assert_eq!(leaf["decay"], json!(4));
    handle.abort();
}

#[tokio::test]
async fn the_cache_facet_of_a_put_leaf_says_put() {
    let (_tmp, mut s, _cache, handle, _sock) = serve().await;
    assert!(put(&mut s, "ns.a", json!("1"), None, None).await.ok);

    let leaf = get(&mut s, "ns:cache.a", None).await.data.unwrap();
    assert!(leaf["age_ms"].is_u64(), "{leaf}");
    assert_eq!(leaf["stale"], json!(false));
    assert_eq!(leaf["source"], json!("put"));
    // A put without a ttl is not a lifecycle entry: no stored keys, no decay.
    for key in ["poll", "watch", "decay"] {
        assert!(leaf.get(key).is_none(), "{key} on a plain put: {leaf}");
    }
    handle.abort();
}

#[tokio::test]
async fn the_cache_facet_of_a_template_answer_has_derived_keys_only() {
    let (_tmp, mut s, _cache, handle, _sock) = serve().await;
    assert!(
        put(
            &mut s,
            "ns:template.greeting",
            json!("hi {{ 1 }}"),
            None,
            None
        )
        .await
        .ok
    );

    let leaf = get(&mut s, "ns:cache.greeting", None).await.data.unwrap();
    assert_eq!(
        leaf,
        json!({"age_ms": 0, "stale": false, "source": "template"})
    );
    handle.abort();
}

#[tokio::test]
async fn the_cache_facet_of_a_miss_is_a_miss() {
    let (_tmp, mut s, _cache, handle, _sock) = serve().await;
    let r = get(&mut s, "nowhere:cache.x", None).await;
    assert!(r.ok && r.data.is_none(), "{r:?}");
    handle.abort();
}

#[tokio::test]
async fn the_cache_facet_pairs_with_the_record_the_read_resolves() {
    let (_tmp, mut s, _cache, handle, _sock) = serve().await;
    let near = TempDir::new().unwrap();
    let far = TempDir::new().unwrap();
    let near = near.path().canonicalize().unwrap();
    let near = near.to_str().unwrap();
    let far = far.path().canonicalize().unwrap();
    let far = far.to_str().unwrap();

    // The global record carries a ttl, the directory's does not: the stored
    // `poll` key says which record answered.
    assert!(
        put(&mut s, "ns.a", json!("global"), Some("60s"), None)
            .await
            .ok
    );
    assert!(
        put(&mut s, "ns.a", json!("near"), None, Some(near))
            .await
            .ok
    );

    let from_near = get(&mut s, "ns:cache.a", Some(near)).await.data.unwrap();
    assert_eq!(from_near["source"], json!("put"));
    assert!(from_near.get("poll").is_none(), "near record: {from_near}");

    let from_far = get(&mut s, "ns:cache.a", Some(far)).await.data.unwrap();
    assert_eq!(from_far["poll"], json!({"interval": "60s", "count": 1}));
    handle.abort();
}

#[tokio::test]
async fn a_ttl_put_reports_its_schedule_and_a_reput_without_one_drops_it() {
    let (_tmp, mut s, _cache, handle, _sock) = serve().await;
    assert!(
        put(&mut s, "build.status", json!("passing"), Some("300s"), None)
            .await
            .ok
    );

    let leaf = get(&mut s, "build:cache.status", None).await.data.unwrap();
    assert_eq!(leaf["poll"], json!({"interval": "300s", "count": 1}));
    assert_eq!(leaf["decay"], json!(1));

    assert!(
        put(&mut s, "build.status", json!("failing"), None, None)
            .await
            .ok
    );
    let leaf = get(&mut s, "build:cache.status", None).await.data.unwrap();
    for key in ["poll", "decay"] {
        assert!(
            leaf.get(key).is_none(),
            "{key} after a plain re-put: {leaf}"
        );
    }

    assert!(
        put(&mut s, "build.status", json!("again"), Some("5s"), None)
            .await
            .ok
    );
    assert!(
        put(&mut s, "build.status", json!(null), None, None)
            .await
            .ok
    );
    let r = get(&mut s, "build:cache.status", None).await;
    assert!(r.data.is_none(), "a removed put leaves nothing: {r:?}");
    handle.abort();
}

#[tokio::test]
async fn a_put_to_a_derived_cache_key_is_refused() {
    let (_tmp, mut s, _cache, handle, _sock) = serve().await;
    for key in ["age_ms", "stale", "source"] {
        let r = put(&mut s, &format!("ns:cache.x.{key}"), json!(1), None, None).await;
        assert!(!r.ok);
        assert_eq!(
            r.error.unwrap(),
            format!("ns:cache.x.{key} is derived and cannot be written")
        );
    }
    for key in ["hash", "watch", "poll.interval", "x"] {
        let r = put(&mut s, &format!("ns:cache.x.{key}"), json!(1), None, None).await;
        assert!(!r.ok);
        assert_eq!(
            r.error.unwrap(),
            format!("ns:cache.x.{key} is written by the daemon")
        );
    }
    handle.abort();
}

#[tokio::test]
async fn a_decay_prototype_put_is_accepted_and_reported() {
    if !has_git() {
        return;
    }
    let (_tmp, mut s, _cache, handle, _sock) = serve().await;
    let repo = GitRepoFixture::new();
    let p = repo.path_str();

    for bad in [json!(0), json!(-1), json!("many"), json!({"a": 1})] {
        let r = put(&mut s, "git:cache._.decay", bad, None, None).await;
        assert!(!r.ok, "a non-positive-integer decay is refused");
    }

    assert!(
        put(&mut s, "*:cache._.decay", json!(3), None, None)
            .await
            .ok
    );
    assert_eq!(
        get(&mut s, "git.branch", Some(p)).await.data.unwrap(),
        "main"
    );
    let leaf = get(&mut s, "git:cache.branch", Some(p)).await.data.unwrap();
    assert_eq!(leaf["decay"], json!(3), "the star prototype: {leaf}");

    // The provider's own prototype beats the star one.
    assert!(
        put(&mut s, "git:cache._.decay", json!(2), None, None)
            .await
            .ok
    );
    let leaf = get(&mut s, "git:cache.branch", Some(p)).await.data.unwrap();
    assert_eq!(leaf["decay"], json!(2), "the provider prototype: {leaf}");

    // The prototype itself reads back.
    let r = get(&mut s, "git:cache._.decay", None).await;
    assert_eq!(r.data.unwrap(), json!(2));
    handle.abort();
}

#[tokio::test]
async fn a_decay_prototype_put_at_a_directory_applies_from_there_down() {
    if !has_git() {
        return;
    }
    let (_tmp, mut s, _cache, handle, _sock) = serve().await;
    let proj = GitRepoFixture::new();
    let other = GitRepoFixture::new();
    let (p, o) = (proj.path_str(), other.path_str());

    assert!(
        put(&mut s, "*:cache._.decay", json!(3), None, None)
            .await
            .ok
    );
    assert!(
        put(&mut s, "git:cache._.decay", json!(6), None, Some(p))
            .await
            .ok
    );
    for dir in [p, o] {
        assert_eq!(
            get(&mut s, "git.branch", Some(dir)).await.data.unwrap(),
            "main"
        );
    }
    let leaf = get(&mut s, "git:cache.branch", Some(p)).await.data.unwrap();
    assert_eq!(leaf["decay"], json!(6), "the directory's prototype: {leaf}");
    let leaf = get(&mut s, "git:cache.branch", Some(o)).await.data.unwrap();
    assert_eq!(leaf["decay"], json!(3), "elsewhere the global one: {leaf}");

    // It reads back where it was put, and is inherited below.
    let r = get(&mut s, "git:cache._.decay", Some(p)).await;
    assert_eq!(r.data.unwrap(), json!(6));
    let r = get(&mut s, "git:cache._.decay", Some(o)).await;
    assert!(r.data.is_none(), "no git prototype at {o}");
    handle.abort();
}

#[tokio::test]
async fn the_cache_facet_of_an_object_node_is_a_tree_of_leaf_answers() {
    let (_tmp, mut s, _cache, handle, _sock) = serve().await;
    assert!(put(&mut s, "ns.a", json!("1"), None, None).await.ok);
    assert!(
        put(
            &mut s,
            "ns.b",
            json!({"c": "2", "d": "3"}),
            Some("60s"),
            None
        )
        .await
        .ok
    );
    assert!(
        put(&mut s, "ns:template.t", json!("tpl"), None, None)
            .await
            .ok
    );

    let whole = get(&mut s, "ns:cache", None).await.data.unwrap();
    assert_eq!(whole["a"]["source"], json!("put"));
    assert!(whole["a"].get("poll").is_none());
    assert_eq!(whole["b"]["c"]["source"], json!("put"));
    assert_eq!(
        whole["b"]["c"]["poll"],
        json!({"interval": "60s", "count": 1}),
        "a leaf below a ttl'd address carries that address's schedule: {whole}"
    );
    assert_eq!(whole["b"]["d"]["decay"], json!(1));
    assert_eq!(
        whole["t"],
        json!({"age_ms": 0, "stale": false, "source": "template"})
    );
    assert!(
        whole.get("_").is_none(),
        "the prototypes are not part of the value"
    );

    // A node returns that part of the tree, and a node's answer is the same
    // tree whether asked for whole or by its address (ages move; drop them).
    let node = get(&mut s, "ns:cache.b", None).await.data.unwrap();
    for leaf in ["c", "d"] {
        let (mut a, mut b) = (node[leaf].clone(), whole["b"][leaf].clone());
        a.as_object_mut().unwrap().remove("age_ms");
        b.as_object_mut().unwrap().remove("age_ms");
        assert_eq!(a, b);
    }
    handle.abort();
}

#[tokio::test]
async fn a_cache_facet_key_cannot_be_refreshed() {
    let (_tmp, mut s, _cache, handle, _sock) = serve().await;
    let r = send(&mut s, json!({"op":"refresh","key":"git:cache.branch"})).await;
    assert!(!r.ok);
    handle.abort();
}

#[tokio::test]
async fn a_watch_on_the_cache_facet_wakes_on_a_data_write() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let writer = cache.clone();
    let handle = Server::new(sock.clone(), cache, registry, None, watchers)
        .spawn()
        .unwrap();

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut w) = stream.into_split();
    let mut reader = BufReader::new(reader);
    w.write_all(b"{\"op\":\"watch\",\"key\":\"nowhere:cache.x\"}\n")
        .await
        .unwrap();
    let first = next_frame(&mut reader).await;
    assert!(first.ok && first.data.is_none(), "{first:?}");

    writer
        .write_static("nowhere", None, "put", "x", Value::String("1".into()), None)
        .unwrap();
    let frame = next_frame(&mut reader).await;
    assert_eq!(frame.data.unwrap()["source"], json!("put"));
    handle.abort();
}

async fn next_frame(reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>) -> Response {
    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        reader.read_line(&mut line),
    )
    .await
    .expect("timed out waiting for a watch frame")
    .unwrap();
    serde_json::from_str(&line).unwrap()
}
