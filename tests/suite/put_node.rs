//! `put` at any node of the tree.
//!
//! A `put` key is `namespace` or `namespace.node…` — the same dotted address
//! `get` takes (`docs/canon/field_resolution.md` invariant 12: a query may
//! address any node of the value tree). Writes deep-merge into whatever is
//! already there, only at the addressed node;
//! `data: null` removes it. There is no ownership: a put layers over
//! whatever a Source produced at the same address.
//!
//! Raw-socket style, mirroring `tests/suite/store.rs` and `tests/suite/key_addressing.rs`.

use crate::common::git::{GitRepoFixture, has_git};
use beachcomber::cache::Cache;
use beachcomber::protocol::Response;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use beachcomber::server::Server;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

type Fixture = (
    TempDir,
    std::path::PathBuf,
    Arc<Cache>,
    Arc<ProviderRegistry>,
    Arc<beachcomber::watcher_registry::WatcherRegistry>,
);

fn setup() -> Fixture {
    setup_with_registry(ProviderRegistry::with_defaults())
}

fn setup_with_registry(registry: ProviderRegistry) -> Fixture {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    (tmp, sock, cache, Arc::new(registry), watchers)
}

/// A Source whose field names are only known once it runs, so it declares a
/// sentinel (`<tool>`) instead — exactly the shape mise, asdf, aws, gcloud and
/// every script/http/library provider have.
struct SentinelSource;

impl Source for SentinelSource {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(|| SourceMetadata {
            name: "tools".into(),
            fields: vec![FieldSchema {
                name: "<tool>".into(),
                field_type: FieldType::String,
            }],
            scope: SourceScope::Global,
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 60,
                count: 2,
            },
            failback: FailbackConfig {
                reattempts: 3,
                interval_secs: 30,
            },
            fsevents_reinstate: false,
        })
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        let mut result = SourceResult::new();
        result.insert("node", Value::String("22.0.0".to_string()));
        result
    }
}

/// How many times [`CountingSource`] ran. Each test gets its own process under
/// nextest, so it starts at zero for every one of them.
static HOST_RUNS: AtomicUsize = AtomicUsize::new(0);

/// A Source that records how many times it ran, so a test can tell "was not
/// re-executed" from "was re-executed to the same value". It declares its one
/// field by name, so a `put` of any other name in the namespace is allowed.
struct CountingSource;

impl Source for CountingSource {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(|| SourceMetadata {
            name: "host".into(),
            fields: vec![FieldSchema {
                name: "name".into(),
                field_type: FieldType::String,
            }],
            scope: SourceScope::Global,
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 60,
                count: 2,
            },
            failback: FailbackConfig {
                reattempts: 3,
                interval_secs: 30,
            },
            fsevents_reinstate: false,
        })
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        HOST_RUNS.fetch_add(1, Ordering::SeqCst);
        let mut result = SourceResult::new();
        result.insert("name", Value::String("counted-host".to_string()));
        result
    }
}

struct CountingProvider;

impl Provider for CountingProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "counted".to_string(),
            sources: vec![CountingSource.metadata().clone()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(CountingSource)]
    }
}

struct SentinelProvider;

impl Provider for SentinelProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "toolset".to_string(),
            sources: vec![SentinelSource.metadata().clone()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(SentinelSource)]
    }
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

// ── (a) a put replaces at its address ──────────────────────────────────────

/// Two puts at the same namespace: the second states the namespace, so `a`
/// is gone. A put one node down replaces only that node.
#[tokio::test]
async fn a_put_replaces_the_existing_value_at_its_address() {
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
    assert_eq!(got, serde_json::json!({"b":2}));

    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"my.c","data":3}),
    )
    .await;
    assert!(r.ok, "put my.c: {:?}", r.error);
    let got = data_of(
        send_recv(&mut s, &serde_json::json!({"op":"get","key":"my"})).await,
        "get my",
    );
    assert_eq!(got, serde_json::json!({"b":2,"c":3}));

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

    // An array is not an object either. The check runs on the wire JSON, before
    // conversion — `Value::from_json` turns an array into an index-keyed object,
    // so checking afterwards would let this through.
    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"my","data":[1,2]}),
    )
    .await;
    assert!(!r.ok, "an array at the root must be rejected");
    assert_eq!(
        r.error.as_deref(),
        Some("put data at the root must be an object")
    );

    handle.abort();
}

// ── key depth is capped ─────────────────────────────────────────────────────

/// A key comes off the socket with no length bound of its own and every
/// segment is one step of a tree walk, so an absurd key must be refused rather
/// than walked. Before the cap a 2000-segment key was accepted and the *next*
/// `get` of the namespace overflowed the stack, taking the daemon and every
/// watch on it down.
#[tokio::test]
async fn put_key_deeper_than_the_cap_is_rejected() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    const TOO_DEEP: &str = "put key is nested too deeply (limit 10 segments)";

    send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"my","data":{"a":1}}),
    )
    .await;

    let absurd = format!("my.{}", vec!["x"; 1999].join("."));
    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":absurd,"data":1}),
    )
    .await;
    assert!(!r.ok, "a 2000-segment key must be rejected");
    assert_eq!(r.error.as_deref(), Some(TOO_DEEP));

    // The daemon is still answering, and the rejected key wrote nothing.
    let got = data_of(
        send_recv(&mut s, &serde_json::json!({"op":"get","key":"my"})).await,
        "get my",
    );
    assert_eq!(got, serde_json::json!({"a":1}));

    // Eleven segments below the namespace is one too many; ten is fine.
    let eleven = format!("deep.{}", ["s"; 11].join("."));
    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":eleven,"data":1}),
    )
    .await;
    assert!(!r.ok, "an 11-segment node must be rejected");
    assert_eq!(r.error.as_deref(), Some(TOO_DEEP));

    let ten = format!("deep.{}", ["s"; 10].join("."));
    let r = send_recv(&mut s, &serde_json::json!({"op":"put","key":ten,"data":1})).await;
    assert!(r.ok, "a 10-segment node must be accepted: {:?}", r.error);
    let got = data_of(
        send_recv(&mut s, &serde_json::json!({"op":"get","key":ten})).await,
        "get the 10-segment node",
    );
    assert_eq!(got, serde_json::json!(1));

    handle.abort();
}

// ── dotted keys in the data are refused ─────────────────────────────────────

/// `{"a.b": 1}` names one record and addresses another: stored naively it
/// becomes `a` → `{b: 1}`, unreachable under the name it was written with.
#[tokio::test]
async fn put_data_with_a_dotted_key_is_rejected() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"dk","data":{"a.b":1}}),
    )
    .await;
    assert!(!r.ok, "a dotted data key must be rejected");
    assert_eq!(
        r.error.as_deref(),
        Some("put data keys may not contain '.': a.b")
    );

    // Nested, and below the root, too.
    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"dk.node","data":{"ok":{"x.y":1}}}),
    )
    .await;
    assert!(!r.ok, "a nested dotted data key must be rejected");
    assert_eq!(
        r.error.as_deref(),
        Some("put data keys may not contain '.': x.y")
    );

    // A refused put registers nothing either, so the namespace still misses.
    let r = send_recv(&mut s, &serde_json::json!({"op":"get","key":"dk"})).await;
    assert!(r.ok && r.data.is_none(), "{r:?}");

    handle.abort();
}

// ── a write with no leaves creates nothing ──────────────────────────────────

/// `put e '{}'` has no records to store. It used to leave an entry carrying an
/// empty `put` map, so `get e` answered `{}` for a namespace nothing had
/// written to.
#[tokio::test]
async fn put_of_an_empty_object_creates_no_entry() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    let r = send_recv(&mut s, &serde_json::json!({"op":"put","key":"e","data":{}})).await;
    assert!(r.ok, "an empty object is a valid no-op put: {:?}", r.error);

    // The namespace is registered (the put named it) but holds nothing, so the
    // read is a miss, not `{}`.
    let r = send_recv(&mut s, &serde_json::json!({"op":"get","key":"e"})).await;
    assert!(r.ok, "get e: {:?}", r.error);
    assert!(r.data.is_none(), "expected a miss, got {:?}", r.data);

    let rows = data_of(
        send_recv(&mut s, &serde_json::json!({"op":"status"})).await,
        "status",
    );
    let mine: Vec<&serde_json::Value> = rows
        .as_array()
        .expect("status data is an array")
        .iter()
        .filter(|r| r["provider"] == "e")
        .collect();
    assert!(mine.is_empty(), "no row for `e`, got {mine:?}");

    // The same write over an existing entry empties it. The
    // label's map is kept for the fill only when there is something to fill it
    // with, so nothing is left behind and the entry goes with it — the write is
    // `put r null` by another name.
    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"r","data":{"a":1}}),
    )
    .await;
    assert!(r.ok, "seed put: {:?}", r.error);
    let r = send_recv(&mut s, &serde_json::json!({"op":"put","key":"r","data":{}})).await;
    assert!(r.ok, "empty put: {:?}", r.error);

    let r = send_recv(&mut s, &serde_json::json!({"op":"get","key":"r"})).await;
    assert!(r.ok, "get r: {:?}", r.error);
    assert!(r.data.is_none(), "expected a miss, got {:?}", r.data);

    let rows = data_of(
        send_recv(&mut s, &serde_json::json!({"op":"status"})).await,
        "status",
    );
    let mine: Vec<&serde_json::Value> = rows
        .as_array()
        .expect("status data is an array")
        .iter()
        .filter(|r| r["provider"] == "r")
        .collect();
    assert!(mine.is_empty(), "no row for `r`, got {mine:?}");

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

// ── (f) a null put at a node removes only that subtree ──────────────────────────

#[tokio::test]
async fn a_null_put_at_a_node_removes_only_that_subtree() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"my","data":{"some":{"value":5},"a":1}}),
    )
    .await;
    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"my.some","data":null}),
    )
    .await;
    assert!(r.ok, "null put at my.some: {:?}", r.error);

    let got = data_of(
        send_recv(&mut s, &serde_json::json!({"op":"get","key":"my"})).await,
        "get my",
    );
    assert_eq!(got, serde_json::json!({"a":1}));

    handle.abort();
}

// ── (g)/(h) a put layers beside a Source's records ────────────────────────────

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
        &serde_json::json!({"op":"put","key":"git.note","data":1,"path":p}),
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

    // Each record names its own supplier: the static one `put`, the Source's
    // still the Source.
    let source_of_note = data_of(
        send_recv(
            &mut s,
            &serde_json::json!({"op":"get","key":"git:cache.note","path":p}),
        )
        .await,
        "get git:cache.note",
    );
    assert_eq!(source_of_note["source"], serde_json::json!("put"));

    let source_of_branch = data_of(
        send_recv(
            &mut s,
            &serde_json::json!({"op":"get","key":"git:cache.branch","path":p}),
        )
        .await,
        "get git:cache.branch",
    );
    assert_eq!(
        source_of_branch["source"],
        serde_json::json!("source(head)")
    );

    handle.abort();
}

/// A put over a Source's field is accepted and wins at that directory: layers,
/// not ownership. The Source's value is still there beneath it.
#[tokio::test]
async fn a_put_over_a_source_field_layers_over_it() {
    if !has_git() {
        return;
    }
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();
    let repo = GitRepoFixture::new();
    let p = repo.path_str();

    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"git","data":{"branch":"x"},"path":p}),
    )
    .await;
    assert!(r.ok, "put git.branch over a Source field: {:?}", r.error);

    let branch = data_of(
        send_recv(
            &mut s,
            &serde_json::json!({"op":"get","key":"git.branch","path":p}),
        )
        .await,
        "get git.branch",
    );
    assert_eq!(branch, serde_json::json!("x"));

    // Removing the put shows the Source's value at once.
    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"git.branch","data":null,"path":p}),
    )
    .await;
    assert!(r.ok, "null put: {:?}", r.error);
    let branch = data_of(
        send_recv(
            &mut s,
            &serde_json::json!({"op":"get","key":"git.branch","path":p}),
        )
        .await,
        "get git.branch",
    );
    assert_eq!(branch, serde_json::json!("main"));

    handle.abort();
}

/// A global put is a default, never an override: the Source at the directory
/// is nearer and wins.
#[tokio::test]
async fn a_nearer_source_beats_a_global_put() {
    if !has_git() {
        return;
    }
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();
    let repo = GitRepoFixture::new();
    let p = repo.path_str();

    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"git.branch","data":"g"}),
    )
    .await;
    assert!(r.ok, "global put: {:?}", r.error);

    let branch = data_of(
        send_recv(
            &mut s,
            &serde_json::json!({"op":"get","key":"git.branch","path":p}),
        )
        .await,
        "get git.branch",
    );
    assert_eq!(branch, serde_json::json!("main"));

    handle.abort();
}

/// A Source with runtime-resolved field names declares a sentinel. A put under
/// it is accepted, warm or cold, and layers over whatever the Source writes.
#[tokio::test]
async fn a_put_under_a_sentinel_source_layers_over_it() {
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(SentinelProvider)).unwrap();
    let (_tmp, sock, cache, registry, watchers) = setup_with_registry(registry);
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    // Cold: nothing cached.
    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"toolset.node","data":"999"}),
    )
    .await;
    assert!(r.ok, "cold put under a sentinel source: {:?}", r.error);

    // Warm the Source with the whole-namespace read; the put still wins.
    let all = data_of(
        send_recv(&mut s, &serde_json::json!({"op":"get","key":"toolset"})).await,
        "get toolset",
    );
    assert_eq!(all["node"], serde_json::json!("999"));

    // A name the Source has never emitted is accepted too.
    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"toolset.note","data":1}),
    )
    .await;
    assert!(r.ok, "put toolset.note: {:?}", r.error);

    // Removing the put shows the Source's value at once.
    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"toolset.node","data":null}),
    )
    .await;
    assert!(r.ok, "null put: {:?}", r.error);
    let node = data_of(
        send_recv(
            &mut s,
            &serde_json::json!({"op":"get","key":"toolset.node"}),
        )
        .await,
        "get toolset.node",
    );
    assert_eq!(node, serde_json::json!("22.0.0"));

    handle.abort();
}

/// A key no Source lists in advance still finds the sentinel Source one node
/// up, so a cold `get` runs it (`mise.node`, PR #11's `public_ip.body`).
#[tokio::test]
async fn a_cold_get_of_a_field_a_sentinel_source_never_declared_runs_it() {
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(SentinelProvider)).unwrap();
    let (_tmp, sock, cache, registry, watchers) = setup_with_registry(registry);
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    let node = data_of(
        send_recv(
            &mut s,
            &serde_json::json!({"op":"get","key":"toolset.node"}),
        )
        .await,
        "cold get toolset.node",
    );
    assert_eq!(node, serde_json::json!("22.0.0"));

    handle.abort();
}

/// A put of an object at a provider's root states the whole namespace at that
/// directory: the Source's unlisted fields are hidden, not removed.
///
/// A counting Source, because a value comparison cannot show "hidden" from
/// "removed and rebuilt": were the put to wipe the `host` map, per-source
/// coldness would re-execute it on the next `get` and bring `name` back. The
/// run count is what tells the two apart.
#[tokio::test]
async fn a_put_object_at_the_provider_node_hides_unlisted_source_fields() {
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(CountingProvider)).unwrap();
    let (_tmp, sock, cache, registry, watchers) = setup_with_registry(registry);
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    let name = data_of(
        send_recv(
            &mut s,
            &serde_json::json!({"op":"get","key":"counted.name"}),
        )
        .await,
        "get counted.name",
    );
    assert_eq!(name, serde_json::json!("counted-host"));
    assert_eq!(HOST_RUNS.load(Ordering::SeqCst), 1, "the cold read ran it");

    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"counted","data":{"note":2}}),
    )
    .await;
    assert!(r.ok, "put at a warm provider root: {:?}", r.error);

    let all = data_of(
        send_recv(&mut s, &serde_json::json!({"op":"get","key":"counted"})).await,
        "get counted",
    );
    assert_eq!(
        all,
        serde_json::json!({"note": 2}),
        "the put states the namespace"
    );
    assert_eq!(
        HOST_RUNS.load(Ordering::SeqCst),
        1,
        "the put left the source's map in place, so the read found it warm"
    );

    // A put of a leaf replaces only that leaf and shows the rest.
    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"counted","data":null}),
    )
    .await;
    assert!(r.ok, "remove the put: {:?}", r.error);
    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"counted.note","data":3}),
    )
    .await;
    assert!(r.ok, "leaf put: {:?}", r.error);
    let all = data_of(
        send_recv(&mut s, &serde_json::json!({"op":"get","key":"counted"})).await,
        "get counted",
    );
    assert_eq!(all["name"], name, "removing the put shows the Source again");
    assert_eq!(all["note"], serde_json::json!(3));
    assert_eq!(HOST_RUNS.load(Ordering::SeqCst), 1);

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

/// `Cache::list_rows` emits one row per put address: a put at `my.some.value`
/// is one row whose field is the address (`some.value`) and whose value is
/// what the put wrote there. This pins that shape.
#[tokio::test]
async fn status_shows_a_nested_put_as_one_row_at_its_address() {
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
    assert_eq!(row["field"], serde_json::json!("some.value"));
    assert_eq!(row["value"], serde_json::json!(5));
    assert_eq!(row["source"], serde_json::json!("put"));
    assert_eq!(row["kind"]["kind"], serde_json::json!("static"));

    handle.abort();
}

// ── (j) a put at the root, then a put below it ────────────────────────────

#[tokio::test]
async fn a_put_below_a_root_put_adds_to_it() {
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
