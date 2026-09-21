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

use crate::common::git::{GitRepoFixture, has_git};
use beachcomber::cache::Cache;
use beachcomber::protocol::Response;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
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
            invalidation: InvalidationStrategy::Poll { interval_secs: 60 },
            keep_alive: KeepAlive::Polls(2),
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
            invalidation: InvalidationStrategy::Poll { interval_secs: 60 },
            keep_alive: KeepAlive::Polls(2),
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

    // A refused put registers nothing either, so the namespace is still unknown.
    let r = send_recv(&mut s, &serde_json::json!({"op":"get","key":"dk"})).await;
    assert_eq!(r.error.as_deref(), Some("unknown provider: dk"));

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

    // The same write with `replace` over an existing entry empties it. The
    // label's map is kept for the fill only when there is something to fill it
    // with, so nothing is left behind and the entry goes with it — the write is
    // `put r --null` by another name.
    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"r","data":{"a":1}}),
    )
    .await;
    assert!(r.ok, "seed put: {:?}", r.error);
    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"r","data":{},"replace":true}),
    )
    .await;
    assert!(r.ok, "empty replace put: {:?}", r.error);

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

    // Ownership stays per record: the static one names `put`, the Source's
    // still names the Source. Gaining a static record does not make the
    // namespace static.
    let source_of_note = data_of(
        send_recv(
            &mut s,
            &serde_json::json!({"op":"get","key":"git.note:source","path":p}),
        )
        .await,
        "get git.note:source",
    );
    assert_eq!(source_of_note, serde_json::json!("put"));

    let source_of_branch = data_of(
        send_recv(
            &mut s,
            &serde_json::json!({"op":"get","key":"git.branch:source","path":p}),
        )
        .await,
        "get git.branch:source",
    );
    assert_eq!(source_of_branch, serde_json::json!("head"));

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

/// A Source with runtime-resolved field names declares a sentinel, so no
/// declared name matches a `put` key and ownership cannot come from
/// `source_for_field`. Such a Source owns every field name in its namespace,
/// warm or cold — otherwise a cold `put toolset.node` lands first and the
/// source's next refresh writes `node` into its own map too, leaving two
/// records at one (key, path) that `get toolset.node` and `get toolset`
/// disagree about.
#[tokio::test]
async fn put_under_a_sentinel_source_is_refused_warm_or_cold() {
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(SentinelProvider)).unwrap();
    let (_tmp, sock, cache, registry, watchers) = setup_with_registry(registry);
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    const EXPECTED: &str = "cannot write toolset.node: owned by source tools";

    // Cold: nothing cached, and `node` is no declared field name.
    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"toolset.node","data":"999"}),
    )
    .await;
    assert!(!r.ok, "cold put under a sentinel source must be refused");
    assert_eq!(r.error.as_deref(), Some(EXPECTED));

    // Warm the source (a sentinel field triggers no cold-miss execute of its
    // own, so the whole-namespace read is what runs it), then try again.
    let all = data_of(
        send_recv(&mut s, &serde_json::json!({"op":"get","key":"toolset"})).await,
        "get toolset",
    );
    assert_eq!(all["node"], serde_json::json!("22.0.0"));

    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"toolset.node","data":"999"}),
    )
    .await;
    assert!(!r.ok, "warm put under a sentinel source must be refused");
    assert_eq!(r.error.as_deref(), Some(EXPECTED));

    // A name the source has never emitted is refused too — a sentinel claims
    // every field name in the namespace, because any of them may show up.
    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"toolset","data":{"note":1}}),
    )
    .await;
    assert!(!r.ok, "a sentinel source owns every field name");
    assert_eq!(
        r.error.as_deref(),
        Some("cannot write toolset.note: owned by source tools")
    );

    // One record, one map.
    let rows = data_of(
        send_recv(&mut s, &serde_json::json!({"op":"status"})).await,
        "status",
    );
    let node_rows: Vec<&serde_json::Value> = rows
        .as_array()
        .expect("status data is an array")
        .iter()
        .filter(|r| r["provider"] == "toolset" && r["field"] == "node")
        .collect();
    assert_eq!(
        node_rows.len(),
        1,
        "exactly one record for toolset.node, got {node_rows:?}"
    );
    assert_eq!(node_rows[0]["source"], serde_json::json!("tools"));

    handle.abort();
}

/// `--replace` acts on static records only. A Source's records under the node
/// are neither removed nor scanned for ownership, so a root `--replace` on a
/// warm provider namespace works — and is symmetric with `--null` there.
///
/// A counting Source, because a value comparison cannot fail here: were
/// `--replace` to wipe the `host` map, per-source coldness would re-execute it
/// on the next `get` and hand back the same string. The run count is what tells
/// "left alone" from "removed and rebuilt".
#[tokio::test]
async fn replace_at_a_provider_root_leaves_the_source_records_alone() {
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
        &serde_json::json!({"op":"put","key":"counted","data":{"note":2},"replace":true}),
    )
    .await;
    assert!(r.ok, "replace at a warm provider root: {:?}", r.error);

    let all = data_of(
        send_recv(&mut s, &serde_json::json!({"op":"get","key":"counted"})).await,
        "get counted",
    );
    assert_eq!(all["name"], name, "the source's record survives --replace");
    assert_eq!(all["note"], serde_json::json!(2));
    assert_eq!(
        HOST_RUNS.load(Ordering::SeqCst),
        1,
        "--replace left the source's map in place, so the read found it warm"
    );

    // A plain put merges over it.
    let r = send_recv(
        &mut s,
        &serde_json::json!({"op":"put","key":"counted","data":{"note":3}}),
    )
    .await;
    assert!(r.ok, "merge put: {:?}", r.error);
    let all = data_of(
        send_recv(&mut s, &serde_json::json!({"op":"get","key":"counted"})).await,
        "get counted",
    );
    assert_eq!(all["name"], name);
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
