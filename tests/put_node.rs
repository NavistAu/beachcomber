//! `put` at any node of the tree.
//!
//! A `put` key is `namespace` or `namespace.node…` — the same dotted address
//! `get` takes (`docs/canon/field_resolution.md` invariant 12: a query may
//! address any node of the value tree). Writes deep-merge into whatever is
//! already there; `--replace` replaces the subtree at the addressed node;
//! `data: null` removes it. Ownership is checked per *record*, not per
//! namespace: `put git '{"note":1}'` is fine, `put git '{"branch":"x"}'` is
//! not, because `branch` is a record the `head` Source refreshes.
//!
//! Raw-socket style, mirroring `tests/store.rs` and `tests/key_addressing.rs`.

mod common;

use beachcomber::cache::Cache;
use beachcomber::protocol::Response;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::server::Server;
use common::git::{GitRepoFixture, has_git};
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

fn data_of(resp: Response, context: &str) -> serde_json::Value {
    assert!(resp.ok, "{context}: expected ok, got {:?}", resp.error);
    resp.data
        .unwrap_or_else(|| panic!("{context}: expected data, got a miss"))
}

// ── (a) default is a deep merge ─────────────────────────────────────────────

/// Two puts at the same namespace merge. Before this change the second put
/// replaced the whole `put` source map and `a` vanished.
#[tokio::test]
async fn put_merges_into_the_existing_namespace() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"my","data":{"a":1}}),
    )
    .await;
    assert!(r.ok, "first put: {:?}", r.error);
    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"my","data":{"b":2}}),
    )
    .await;
    assert!(r.ok, "second put: {:?}", r.error);

    let got = data_of(
        send_recv(&mut s, &serde_json::json!({"op":"get","key":"my"})).await,
        "get my",
    );
    assert_eq!(got, serde_json::json!({"a":1,"b":2}));

    handle.abort();
}

// ── (b) replace at the root ─────────────────────────────────────────────────

#[tokio::test]
async fn put_replace_drops_the_previous_subtree() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"my","data":{"a":1}}),
    )
    .await;
    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"my","data":{"b":2},"replace":true}),
    )
    .await;
    assert!(r.ok, "replace put: {:?}", r.error);

    let got = data_of(
        send_recv(&mut s, &serde_json::json!({"op":"get","key":"my"})).await,
        "get my",
    );
    assert_eq!(got, serde_json::json!({"b":2}));

    handle.abort();
}

// ── (c) a dotted key addresses a node, not a namespace with dots in it ──────

/// Before this change `put my.some.value 5` created a namespace literally
/// named `my.some.value` that no `get` could reach.
#[tokio::test]
async fn put_at_a_nested_node_is_readable_at_every_level() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"my.some.value","data":5}),
    )
    .await;
    assert!(r.ok, "put my.some.value: {:?}", r.error);

    let leaf = data_of(
        send_recv(
            &mut s,
            &serde_json::json!({"op":"get","key":"my.some.value"}),
        )
        .await,
        "get my.some.value",
    );
    assert_eq!(leaf, serde_json::json!(5));

    let mid = data_of(
        send_recv(&mut s, &serde_json::json!({"op":"get","key":"my.some"})).await,
        "get my.some",
    );
    assert_eq!(mid, serde_json::json!({"value":5}));

    let root = data_of(
        send_recv(&mut s, &serde_json::json!({"op":"get","key":"my"})).await,
        "get my",
    );
    assert_eq!(root, serde_json::json!({"some":{"value":5}}));

    handle.abort();
}

// ── (d) the root still has to be an object ──────────────────────────────────

#[tokio::test]
async fn put_scalar_at_the_root_is_rejected() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"my","data":"x"}),
    )
    .await;
    assert!(!r.ok, "a scalar at the root must be rejected");
    assert_eq!(
        r.error.as_deref(),
        Some("put data at the root must be an object")
    );

    handle.abort();
}

// ── (e) below the root, any JSON goes ───────────────────────────────────────

#[tokio::test]
async fn put_below_the_root_accepts_any_json() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"my.s","data":"x"}),
    )
    .await;
    assert!(r.ok, "put my.s: {:?}", r.error);
    let got = data_of(
        send_recv(&mut s, &serde_json::json!({"op":"get","key":"my.s"})).await,
        "get my.s",
    );
    assert_eq!(got, serde_json::json!("x"));

    // An array becomes an index-keyed object, as everywhere else in the tree.
    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"my.arr","data":[1,2]}),
    )
    .await;
    assert!(r.ok, "put my.arr: {:?}", r.error);
    let got = data_of(
        send_recv(&mut s, &serde_json::json!({"op":"get","key":"my.arr.0"})).await,
        "get my.arr.0",
    );
    assert_eq!(got, serde_json::json!(1));

    handle.abort();
}

// ── (f) --null at a node removes only that subtree ──────────────────────────

#[tokio::test]
async fn put_null_at_a_node_removes_only_that_subtree() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"my","data":{"some":{"value":5},"a":1}}),
    )
    .await;
    let r = send_recv(&mut s, &serde_json::json!({"op":"put","key":"my.some"})).await;
    assert!(r.ok, "null put at my.some: {:?}", r.error);

    let got = data_of(
        send_recv(&mut s, &serde_json::json!({"op":"get","key":"my"})).await,
        "get my",
    );
    assert_eq!(got, serde_json::json!({"a":1}));

    handle.abort();
}

// ── (g)/(h) ownership is per record, not per namespace ──────────────────────

/// A record no Source declares can be stored under a real provider's
/// namespace, and it reads back beside the provider's own records.
#[tokio::test]
async fn put_adds_a_static_record_to_a_provider_namespace() {
    if !has_git() {
        return;
    }
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    let repo = GitRepoFixture::new();
    let p = repo.path_str();

    // Warm the `head` source at this path.
    let branch = data_of(
        send_recv(
            &mut s,
            &serde_json::json!({"op":"get","key":"git.branch","path":p}),
        )
        .await,
        "get git.branch",
    );
    assert_eq!(branch, serde_json::json!("main"));

    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"git","data":{"note":1},"path":p}),
    )
    .await;
    assert!(r.ok, "put git.note: {:?}", r.error);

    let note = data_of(
        send_recv(
            &mut s,
            &serde_json::json!({"op":"get","key":"git.note","path":p}),
        )
        .await,
        "get git.note",
    );
    assert_eq!(note, serde_json::json!(1));

    // The whole-namespace read deep-merges the `head` and `put` records.
    let all = data_of(
        send_recv(
            &mut s,
            &serde_json::json!({"op":"get","key":"git","path":p}),
        )
        .await,
        "get git",
    );
    assert_eq!(all["branch"], serde_json::json!("main"));
    assert_eq!(all["note"], serde_json::json!(1));

    // The namespace is still the git provider's — gaining a static record does
    // not turn it into a static namespace.
    let kind = data_of(
        send_recv(
            &mut s,
            &serde_json::json!({"op":"get","key":"git:source","path":p}),
        )
        .await,
        "get git:source",
    );
    assert_eq!(kind, serde_json::json!("builtin"));

    handle.abort();
}

/// A record a Source declares is refused, whichever way it is addressed —
/// including by `--null`. No warming needed: ownership comes from the
/// registry, not from what happens to be cached.
#[tokio::test]
async fn put_over_a_source_owned_record_is_refused() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    const EXPECTED: &str = "cannot write git.branch: owned by source head";

    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"git","data":{"branch":"x"}}),
    )
    .await;
    assert!(!r.ok, "put git '{{branch}}' must be refused");
    assert_eq!(r.error.as_deref(), Some(EXPECTED));

    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"git.branch","data":"x"}),
    )
    .await;
    assert!(!r.ok, "put git.branch must be refused");
    assert_eq!(r.error.as_deref(), Some(EXPECTED));

    let r = send_recv(&mut s, &serde_json::json!({"op":"put","key":"git.branch"})).await;
    assert!(!r.ok, "null put at git.branch must be refused");
    assert_eq!(r.error.as_deref(), Some(EXPECTED));

    handle.abort();
}

/// A facet namespace is an ordinary namespace with a colon in it, so it takes
/// a node the same way. The key splits on its first dot, never on the colon.
#[tokio::test]
async fn put_at_a_node_under_a_facet_namespace() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"scanline:template.widgets.clock","data":"%H:%M"}),
    )
    .await;
    assert!(r.ok, "put at a facet node: {:?}", r.error);

    let got = data_of(
        send_recv(
            &mut s,
            &serde_json::json!({"op":"get","key":"scanline:template.widgets"}),
        )
        .await,
        "get scanline:template.widgets",
    );
    assert_eq!(got, serde_json::json!({"clock":"%H:%M"}));

    handle.abort();
}

// ── (i) status shows the static record ──────────────────────────────────────

/// `Cache::list_rows` emits one row per *top-level* field of each source map,
/// so a nested `put` shows up as a single row for the top-level object node
/// (`field == "some"`, value the subtree) rather than one row per leaf. This
/// pins that shape.
#[tokio::test]
async fn status_shows_a_nested_put_as_one_static_row() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"my.some.value","data":5}),
    )
    .await;

    let rows = data_of(
        send_recv(&mut s, &serde_json::json!({"op":"status"})).await,
        "status",
    );
    let rows = rows.as_array().expect("status data is an array");
    let mine: Vec<&serde_json::Value> = rows.iter().filter(|r| r["provider"] == "my").collect();
    assert_eq!(mine.len(), 1, "exactly one row for `my`, got {mine:?}");
    let row = mine[0];
    assert_eq!(row["field"], serde_json::json!("some"));
    assert_eq!(row["value"], serde_json::json!({"value":5}));
    assert_eq!(row["source"], serde_json::json!("put"));
    assert_eq!(row["kind"]["kind"], serde_json::json!("static"));

    handle.abort();
}

// ── (j) replace at the root, then merge below it ────────────────────────────

#[tokio::test]
async fn replace_at_the_root_then_merge_below_it() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"my","data":{"a":1},"replace":true}),
    )
    .await;
    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"my.b","data":2}),
    )
    .await;
    assert!(r.ok, "put my.b: {:?}", r.error);

    let got = data_of(
        send_recv(&mut s, &serde_json::json!({"op":"get","key":"my"})).await,
        "get my",
    );
    assert_eq!(got, serde_json::json!({"a":1,"b":2}));

    handle.abort();
}
