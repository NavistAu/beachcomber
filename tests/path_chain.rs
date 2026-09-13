//! `$HOME` is the global path, and a read resolves static records up the
//! directory chain (canon `field_resolution.md` §"Path resolution").
//!
//! The chain for a path `P` is `P`, each ancestor below `$HOME` (or every one
//! up to `/` when `P` is outside `$HOME`), then the global entry — which is
//! `$HOME`, so `$HOME` is never a link of its own. Static records — those whose
//! source is `put` or `config:<file>` — are collected from every entry on the
//! chain, the nearest winning. Provider-produced records keep their Source's
//! declared scope: a `Global` source is read at the global entry, a
//! `PathScoped` one at the addressed path; neither walks the chain.
//!
//! Each test owns its `$HOME`: nextest runs every test in its own process, so
//! `set_var("HOME", …)` here is process-local. That assumption is nextest's,
//! not cargo's — under `NEXTEST=1 cargo test` these share one process and one
//! `$HOME`, and `query::home_dir`'s `OnceLock` freezes whichever won the race.

mod common;

use beachcomber::cache::Cache;
use beachcomber::protocol::Response;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use beachcomber::server::Server;
use common::git::{GitRepoFixture, has_git};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

// ── fixture ──────────────────────────────────────────────────────────────────

struct Fixture {
    _sock_dir: TempDir,
    _root: TempDir,
    sock: PathBuf,
    /// The temp root; `$HOME` is it, or a directory below it.
    root: PathBuf,
    home: PathBuf,
    cache: Arc<Cache>,
}

impl Fixture {
    /// `$HOME` is a fresh temp root holding `ws/project/foo/bar/baz` and
    /// `ws/other`. The server runs in-process on a unix socket.
    fn start(registry: ProviderRegistry) -> Fixture {
        Fixture::start_with_home(registry, "")
    }

    /// As [`Fixture::start`], with `$HOME` at `home_rel` below the temp root
    /// — so a test can place something *above* `$HOME`.
    fn start_with_home(registry: ProviderRegistry, home_rel: &str) -> Fixture {
        let root_dir = TempDir::new().unwrap();
        let root = root_dir.path().canonicalize().unwrap();
        // `root.join("")` would append a trailing separator; `$HOME` at the
        // root itself must be the canonical path, byte for byte.
        let home = if home_rel.is_empty() {
            root.clone()
        } else {
            root.join(home_rel)
        };
        std::fs::create_dir_all(&home).unwrap();
        // SAFETY: nextest gives every test its own process, so no other thread
        // is reading the environment concurrently.
        unsafe { std::env::set_var("HOME", &home) };

        std::fs::create_dir_all(home.join("ws/project/foo/bar/baz")).unwrap();
        std::fs::create_dir_all(home.join("ws/other")).unwrap();

        let sock_dir = TempDir::new().unwrap();
        let sock = sock_dir.path().join("test.sock");
        let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
        let cache = Arc::new(Cache::with_watchers(watchers.clone()));
        let server = Server::new(
            sock.clone(),
            cache.clone(),
            Arc::new(registry),
            None,
            watchers,
        );
        server.spawn().unwrap();

        Fixture {
            _sock_dir: sock_dir,
            _root: root_dir,
            sock,
            root,
            home,
            cache,
        }
    }

    fn defaults() -> Fixture {
        Fixture::start(ProviderRegistry::with_defaults())
    }

    fn path(&self, rel: &str) -> String {
        self.home.join(rel).to_string_lossy().into_owned()
    }

    async fn connect(&self) -> UnixStream {
        UnixStream::connect(&self.sock).await.unwrap()
    }
}

async fn send_recv(stream: &mut UnixStream, request: serde_json::Value) -> Response {
    let mut line = request.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes()).await.unwrap();
    let mut reader = BufReader::new(&mut *stream);
    let mut resp = String::new();
    reader.read_line(&mut resp).await.unwrap();
    serde_json::from_str(&resp).unwrap()
}

fn data(resp: Response, what: &str) -> serde_json::Value {
    assert!(resp.ok, "{what}: expected ok, got {:?}", resp.error);
    resp.data
        .unwrap_or_else(|| panic!("{what}: expected data, got a miss"))
}

fn put(key: &str, value: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"op": "put", "key": key, "data": value})
}

fn put_at(key: &str, value: serde_json::Value, path: &str) -> serde_json::Value {
    serde_json::json!({"op": "put", "key": key, "data": value, "path": path})
}

fn get(key: &str) -> serde_json::Value {
    serde_json::json!({"op": "get", "key": key})
}

fn get_at(key: &str, path: &str) -> serde_json::Value {
    serde_json::json!({"op": "get", "key": key, "path": path})
}

// ── (a) nearest wins, farther layers still show through ──────────────────────

#[tokio::test]
async fn static_records_resolve_up_the_chain() {
    let f = Fixture::defaults();
    let mut s = f.connect().await;

    let resp = send_recv(&mut s, put("my", serde_json::json!({"a": 1, "b": 1}))).await;
    assert!(resp.ok, "global put: {:?}", resp.error);
    let resp = send_recv(
        &mut s,
        put_at("my", serde_json::json!({"b": 2}), &f.path("ws/project")),
    )
    .await;
    assert!(resp.ok, "project put: {:?}", resp.error);

    let deep = f.path("ws/project/foo/bar/baz");
    assert_eq!(
        data(send_recv(&mut s, get_at("my", &deep)).await, "deep read"),
        serde_json::json!({"a": 1, "b": 2}),
        "the project record shadows the global one; the global `a` still shows through"
    );

    let other = f.path("ws/other");
    assert_eq!(
        data(
            send_recv(&mut s, get_at("my", &other)).await,
            "sibling read"
        ),
        serde_json::json!({"a": 1, "b": 1}),
        "a sibling branch of the tree sees only the global record"
    );

    assert_eq!(
        data(send_recv(&mut s, get("my")).await, "global read"),
        serde_json::json!({"a": 1, "b": 1}),
        "--global reads the global entry alone"
    );

    assert_eq!(
        data(send_recv(&mut s, get_at("my.b", &deep)).await, "deep field"),
        serde_json::json!(2),
        "a field read resolves through the same chain"
    );
}

// ── (b) removing the nearer record uncovers the farther one ──────────────────

#[tokio::test]
async fn clearing_a_nearer_record_uncovers_the_global_one() {
    let f = Fixture::defaults();
    let mut s = f.connect().await;

    send_recv(&mut s, put("my", serde_json::json!({"a": 1, "b": 1}))).await;
    send_recv(
        &mut s,
        put_at("my", serde_json::json!({"b": 2}), &f.path("ws/project")),
    )
    .await;

    let deep = f.path("ws/project/foo/bar/baz");
    assert_eq!(
        data(send_recv(&mut s, get_at("my.b", &deep)).await, "shadowed"),
        serde_json::json!(2)
    );

    let clear = serde_json::json!({
        "op": "put", "key": "my.b", "data": null, "path": f.path("ws/project"),
    });
    let resp = send_recv(&mut s, clear).await;
    assert!(resp.ok, "clear: {:?}", resp.error);

    assert_eq!(
        data(send_recv(&mut s, get_at("my.b", &deep)).await, "uncovered"),
        serde_json::json!(1),
        "with the project record gone the global one is visible again"
    );
}

// ── (c) $HOME is the global path ─────────────────────────────────────────────

#[tokio::test]
async fn a_put_at_home_lands_in_the_global_entry() {
    let f = Fixture::defaults();
    let mut s = f.connect().await;

    let home = f.home.to_string_lossy().into_owned();
    let resp = send_recv(&mut s, put_at("my", serde_json::json!({"c": 3}), &home)).await;
    assert!(resp.ok, "put at $HOME: {:?}", resp.error);

    assert_eq!(
        data(send_recv(&mut s, get("my.c")).await, "pathless read"),
        serde_json::json!(3),
        "a request with no path field reads the entry a $HOME put wrote"
    );
    assert_eq!(
        data(send_recv(&mut s, get_at("my.c", &home)).await, "$HOME read"),
        serde_json::json!(3),
        "addressing $HOME explicitly reads the same entry"
    );
    let rows = data(
        send_recv(&mut s, serde_json::json!({"op": "status"})).await,
        "status",
    );
    let paths: std::collections::BTreeSet<String> = rows
        .as_array()
        .expect("status returns an array")
        .iter()
        .filter(|row| row["provider"] == "my")
        .map(|row| row["path"].to_string())
        .collect();
    assert_eq!(
        paths,
        std::collections::BTreeSet::from(["null".to_string()]),
        "the record is keyed pathless — no separate $HOME-keyed entry exists"
    );
}

// ── (d) a path outside $HOME still ends at the global entry ──────────────────

#[tokio::test]
async fn a_path_outside_home_still_sees_the_global_record() {
    let f = Fixture::defaults();
    let outside = TempDir::new().unwrap();
    let deep = outside.path().canonicalize().unwrap().join("x/y");
    std::fs::create_dir_all(&deep).unwrap();
    let deep = deep.to_string_lossy().into_owned();

    let mut s = f.connect().await;
    send_recv(&mut s, put("my", serde_json::json!({"v": "global"}))).await;

    assert_eq!(
        data(send_recv(&mut s, get_at("my.v", &deep)).await, "outside"),
        serde_json::json!("global"),
        "a path outside $HOME walks up to / and then the global entry"
    );
}

// ── (e)+(g) provider records keep their declared scope ───────────────────────

/// A Source that never runs — every test that uses it seeds the cache
/// directly, so what is asserted is the read's layering, not execution.
struct InertSource(SourceMetadata);

impl Source for InertSource {
    fn metadata(&self) -> &SourceMetadata {
        &self.0
    }
    fn execute(&self, _path: Option<&str>) -> SourceResult {
        SourceResult::new()
    }
}

fn source_meta(name: &str, field: &str, scope: SourceScope) -> SourceMetadata {
    SourceMetadata {
        name: name.into(),
        fields: vec![FieldSchema {
            name: field.into(),
            field_type: FieldType::String,
        }],
        scope,
        invalidation: InvalidationStrategy::Poll { interval_secs: 60 },
        keep_alive: KeepAlive::Polls(2),
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 30,
        },
        fsevents_reinstate: false,
    }
}

/// `p` has one `Global` source and one `PathScoped` one — mise's shape.
struct TwoScopeProvider;

impl Provider for TwoScopeProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "p".to_string(),
            sources: vec![
                source_meta("glob", "gv", SourceScope::Global),
                source_meta("proj", "pv", SourceScope::PathScoped),
            ],
        }
    }
    fn sources(&self) -> Vec<Box<dyn Source>> {
        self.metadata()
            .sources
            .into_iter()
            .map(|m| Box::new(InertSource(m)) as Box<dyn Source>)
            .collect()
    }
}

fn two_scope_registry() -> ProviderRegistry {
    let mut reg = ProviderRegistry::new();
    reg.register(Box::new(TwoScopeProvider)).unwrap();
    reg
}

fn fields(pairs: Vec<(&str, Value)>) -> HashMap<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

// ── coldness and `--force` are decided per source ────────────────────────────

// One counter per source. Each test runs in its own process, so these start at
// zero for every one of them.
static GLOB_RUNS: AtomicUsize = AtomicUsize::new(0);
static PROJ_RUNS: AtomicUsize = AtomicUsize::new(0);

/// A Source that records how many times it ran and reports the count, so a test
/// can tell "was not re-executed" from "was re-executed to the same value".
struct CountingSource {
    meta: SourceMetadata,
    runs: &'static AtomicUsize,
    field: &'static str,
}

impl Source for CountingSource {
    fn metadata(&self) -> &SourceMetadata {
        &self.meta
    }
    fn execute(&self, _path: Option<&str>) -> SourceResult {
        let n = self.runs.fetch_add(1, Ordering::SeqCst) + 1;
        let mut result = SourceResult::new();
        result.insert(self.field, Value::Int(n as i64));
        result
    }
}

/// `c` has the same two-scope shape as `p`, with sources that actually produce
/// records — so they go warm, and a second execution is observable.
struct CountingProvider;

impl Provider for CountingProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "c".to_string(),
            sources: vec![
                source_meta("glob", "gv", SourceScope::Global),
                source_meta("proj", "pv", SourceScope::PathScoped),
            ],
        }
    }
    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![
            Box::new(CountingSource {
                meta: source_meta("glob", "gv", SourceScope::Global),
                runs: &GLOB_RUNS,
                field: "gv",
            }),
            Box::new(CountingSource {
                meta: source_meta("proj", "pv", SourceScope::PathScoped),
                runs: &PROJ_RUNS,
                field: "pv",
            }),
        ]
    }
}

fn counting_registry() -> ProviderRegistry {
    let mut reg = ProviderRegistry::new();
    reg.register(Box::new(CountingProvider)).unwrap();
    reg
}

fn get_forced(key: &str, path: Option<&str>) -> serde_json::Value {
    match path {
        Some(p) => serde_json::json!({"op": "get", "key": key, "path": p, "force": true}),
        None => serde_json::json!({"op": "get", "key": key, "force": true}),
    }
}

fn get_waited(key: &str, path: Option<&str>) -> serde_json::Value {
    match path {
        Some(p) => serde_json::json!({"op": "get", "key": key, "path": p, "wait": true}),
        None => serde_json::json!({"op": "get", "key": key, "wait": true}),
    }
}

/// A warm sibling is not evidence that this Source is warm. The `Global` source
/// has records and the `PathScoped` one does not, so the read must run the one
/// that is actually cold and return both.
#[tokio::test]
async fn a_cold_source_runs_even_when_its_sibling_is_warm() {
    let f = Fixture::start(counting_registry());
    let project = f.path("ws/project");
    f.cache
        .put_source("c", None, "glob", fields(vec![("gv", Value::Int(0))]), None);

    let mut s = f.connect().await;
    let whole = data(send_recv(&mut s, get_at("c", &project)).await, "cold path");

    assert_eq!(
        whole["gv"],
        serde_json::json!(0),
        "the warm global source was left alone"
    );
    assert_eq!(
        whole["pv"],
        serde_json::json!(1),
        "the cold path-scoped source ran"
    );
    assert_eq!(GLOB_RUNS.load(Ordering::SeqCst), 0);
    assert_eq!(PROJ_RUNS.load(Ordering::SeqCst), 1);
}

/// `--force` re-runs every source the read returns, each at its own scope's
/// entry, and evicts none of them wholesale — the static records sharing the
/// addressed entry are still there afterwards.
#[tokio::test]
async fn force_re_executes_every_source_and_keeps_the_static_records() {
    let f = Fixture::start(counting_registry());
    let project = f.path("ws/project");
    let mut s = f.connect().await;

    let first = data(send_recv(&mut s, get_at("c", &project)).await, "first read");
    assert_eq!(first["gv"], serde_json::json!(1));
    assert_eq!(first["pv"], serde_json::json!(1));

    let resp = send_recv(
        &mut s,
        put_at("c.note", serde_json::json!("keep"), &project),
    )
    .await;
    assert!(resp.ok, "put beside the provider records: {:?}", resp.error);

    let forced = data(
        send_recv(&mut s, get_forced("c", Some(&project))).await,
        "forced read",
    );
    assert_eq!(
        forced["gv"],
        serde_json::json!(2),
        "the global source re-ran at the global entry, which the request did not name"
    );
    assert_eq!(
        forced["pv"],
        serde_json::json!(2),
        "the path-scoped source re-ran at the addressed entry"
    );
    assert_eq!(
        forced["note"],
        serde_json::json!("keep"),
        "--force refreshes records, it does not evict the entry they share"
    );

    // A field read forces its owning source only.
    let pv = data(
        send_recv(&mut s, get_forced("c.pv", Some(&project))).await,
        "forced field",
    );
    assert_eq!(pv, serde_json::json!(3));
    assert_eq!(
        GLOB_RUNS.load(Ordering::SeqCst),
        2,
        "the global source is untouched by a forced read of a path-scoped field"
    );
}

/// `--force --global` reaches the global source at the entry it lives at.
#[tokio::test]
async fn force_at_the_global_entry_re_executes_the_global_source() {
    let f = Fixture::start(counting_registry());
    let mut s = f.connect().await;

    let first = data(send_recv(&mut s, get("c")).await, "first --global read");
    assert_eq!(first["gv"], serde_json::json!(1));

    let forced = data(
        send_recv(&mut s, get_forced("c", None)).await,
        "forced --global read",
    );
    assert_eq!(
        forced["gv"],
        serde_json::json!(2),
        "--force --global re-runs the global source"
    );
    assert_eq!(GLOB_RUNS.load(Ordering::SeqCst), 2);
}

// ── `--wait` is decided per source too ───────────────────────────────────────

/// The sources of `counting_registry` declare a 60s poll interval, so a warm
/// map is stale once the mock clock is past it. Advancing rather than sleeping
/// keeps the test instant.
async fn age_past_the_poll_interval() {
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::time::resume();
}

/// `--wait` re-executes the stale source and leaves everything else at the
/// entry alone. Canon `cache-lifecycle.md` lets only `put --null` and daemon
/// exit remove a static record; `--wait` used to evict the whole entry and take
/// them with it.
#[tokio::test]
async fn wait_re_executes_a_stale_source_and_keeps_the_static_records() {
    let f = Fixture::start(counting_registry());
    let project = f.path("ws/project");
    let mut s = f.connect().await;

    let first = data(send_recv(&mut s, get_at("c", &project)).await, "first read");
    assert_eq!(first["pv"], serde_json::json!(1));

    let resp = send_recv(
        &mut s,
        put_at("c.note", serde_json::json!("keep"), &project),
    )
    .await;
    assert!(resp.ok, "put beside the provider records: {:?}", resp.error);

    age_past_the_poll_interval().await;

    let waited = data(
        send_recv(&mut s, get_waited("c", Some(&project))).await,
        "waited read",
    );
    assert_eq!(
        PROJ_RUNS.load(Ordering::SeqCst),
        2,
        "the stale path-scoped source ran again"
    );
    assert_eq!(waited["pv"], serde_json::json!(2));
    assert_eq!(
        waited["note"],
        serde_json::json!("keep"),
        "--wait refreshes a source's map, it does not evict the entry it shares"
    );
}

/// A `Global` source is stale at the global entry, and the request names a
/// directory. `--wait` used to look for staleness at the addressed entry — where
/// that source keeps nothing — and so never refreshed it.
#[tokio::test]
async fn wait_re_executes_a_stale_global_source_from_a_directory_read() {
    let f = Fixture::start(counting_registry());
    let project = f.path("ws/project");
    let mut s = f.connect().await;

    let first = data(send_recv(&mut s, get_at("c", &project)).await, "first read");
    assert_eq!(first["gv"], serde_json::json!(1));

    age_past_the_poll_interval().await;

    let waited = data(
        send_recv(&mut s, get_waited("c", Some(&project))).await,
        "waited read",
    );
    assert_eq!(
        GLOB_RUNS.load(Ordering::SeqCst),
        2,
        "the global source ran again even though the request named a directory"
    );
    assert_eq!(waited["gv"], serde_json::json!(2));
    assert_eq!(
        f.cache
            .get_source("c", None, "glob")
            .expect("the global source keeps its map at the global entry")
            .fields["gv"],
        Value::Int(2),
        "the refreshed records landed at the global entry, not the addressed one"
    );
}

/// Freshness is what `--wait` waits on. A source still inside its interval is
/// read, not re-run.
#[tokio::test]
async fn wait_leaves_a_fresh_source_alone() {
    let f = Fixture::start(counting_registry());
    let project = f.path("ws/project");
    let mut s = f.connect().await;

    data(send_recv(&mut s, get_at("c", &project)).await, "first read");

    let waited = data(
        send_recv(&mut s, get_waited("c", Some(&project))).await,
        "waited read",
    );
    assert_eq!(waited["gv"], serde_json::json!(1));
    assert_eq!(waited["pv"], serde_json::json!(1));
    assert_eq!(GLOB_RUNS.load(Ordering::SeqCst), 1);
    assert_eq!(PROJ_RUNS.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_provider_record_is_read_at_its_sources_scope() {
    let f = Fixture::start(two_scope_registry());
    let project = f.path("ws/project");

    f.cache.put_source(
        "p",
        None,
        "glob",
        fields(vec![
            ("v", Value::String("global-v".into())),
            ("gv", Value::String("G".into())),
        ]),
        None,
    );
    f.cache.put_source(
        "p",
        Some(&project),
        "proj",
        fields(vec![
            ("v", Value::String("project-v".into())),
            ("pv", Value::String("P".into())),
        ]),
        None,
    );

    let mut s = f.connect().await;

    assert_eq!(
        data(send_recv(&mut s, get_at("p.v", &project)).await, "project"),
        serde_json::json!("project-v"),
        "at a project path the path-scoped record wins over the global one"
    );
    assert_eq!(
        data(send_recv(&mut s, get("p.v")).await, "global"),
        serde_json::json!("global-v"),
        "--global reads the global source's record"
    );

    let whole = data(send_recv(&mut s, get_at("p", &project)).await, "whole");
    assert_eq!(whole["gv"], serde_json::json!("G"), "global-only field");
    assert_eq!(whole["pv"], serde_json::json!("P"), "path-only field");
    assert_eq!(whole["v"], serde_json::json!("project-v"));
}

/// Both scopes' maps can sit on the global entry at once — a path-scoped source
/// runs at `$HOME` for that address, so its records land there beside the
/// global source's. `--global` must answer with the `Global` source's record,
/// and must answer the same way every time: the merge order inside a layer is
/// fixed, not whatever a HashMap iterates.
#[tokio::test]
async fn the_global_source_wins_at_the_global_entry() {
    let f = Fixture::start(two_scope_registry());
    let project = f.path("ws/project");

    f.cache.put_source(
        "p",
        None,
        "proj",
        fields(vec![("v", Value::String("path-scoped-at-home".into()))]),
        None,
    );
    f.cache.put_source(
        "p",
        None,
        "glob",
        fields(vec![("v", Value::String("global-v".into()))]),
        None,
    );
    f.cache.put_source(
        "p",
        Some(&project),
        "proj",
        fields(vec![("v", Value::String("project-v".into()))]),
        None,
    );

    let mut s = f.connect().await;
    for attempt in 1..=2 {
        assert_eq!(
            data(send_recv(&mut s, get("p.v")).await, "--global"),
            serde_json::json!("global-v"),
            "attempt {attempt}: --global resolves to the Global source's record"
        );
    }
    assert_eq!(
        data(send_recv(&mut s, get_at("p.v", &project)).await, "project"),
        serde_json::json!("project-v"),
        "at a project path the path-scoped record still wins"
    );
}

/// A provider record and a static record inside the same object node: the read
/// stitches them together instead of letting one map answer alone.
///
/// The two halves live on different layers — the `Global` source's at the
/// global entry, the static one at the project directory — so the stitching
/// happens across the chain, not inside one entry.
#[tokio::test]
async fn a_static_record_merges_into_a_provider_produced_object() {
    let f = Fixture::start(two_scope_registry());
    let project = f.path("ws/project");

    f.cache.put_source(
        "p",
        None,
        "glob",
        fields(vec![(
            "tools",
            Value::Object(fields(vec![("y", Value::Int(2))])),
        )]),
        None,
    );

    let mut s = f.connect().await;
    let resp = send_recv(&mut s, put_at("p.tools.x", serde_json::json!(1), &project)).await;
    assert!(resp.ok, "put beside a provider record: {:?}", resp.error);

    assert_eq!(
        data(
            send_recv(&mut s, get_at("p.tools", &project)).await,
            "interior object"
        ),
        serde_json::json!({"x": 1, "y": 2}),
        "the static and provider halves of one object node both show up"
    );
    assert_eq!(
        data(send_recv(&mut s, get("p.tools")).await, "global read"),
        serde_json::json!({"y": 2}),
        "the static half really is a layer down — the global entry holds only \
         the provider's half"
    );
}

// ── a narrowing provider still reads static records below its entry ──────────

/// A `PathScoped` source that walks up to the nearest ancestor holding a
/// `.root` marker — git's shape (`walk_to_git`) without needing a repository.
/// It reports the directory it ran in, so a read proves where the record lives,
/// and the marker's text as `tag` when there is any, so two roots can produce
/// different field sets.
struct RootedSource(SourceMetadata);

impl Source for RootedSource {
    fn metadata(&self) -> &SourceMetadata {
        &self.0
    }
    fn execute(&self, path: Option<&str>) -> SourceResult {
        let mut result = SourceResult::new();
        let dir = path.unwrap_or_default();
        result.insert("field", Value::String(dir.to_string()));
        if let Ok(tag) = std::fs::read_to_string(Path::new(dir).join(".root"))
            && !tag.is_empty()
        {
            result.insert("tag", Value::String(tag));
        }
        result
    }
    fn canonical_path(&self, path: Option<&str>) -> Option<String> {
        let mut cur = Some(Path::new(path?));
        while let Some(dir) = cur {
            if dir.join(".root").exists() {
                return Some(dir.to_string_lossy().into_owned());
            }
            cur = dir.parent();
        }
        None
    }
}

/// `r` narrows the requested path to the marker directory, so its records sit
/// above where a bare `put` from a subdirectory lands.
struct RootedProvider;

impl Provider for RootedProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "r".to_string(),
            sources: vec![source_meta("tree", "field", SourceScope::PathScoped)],
        }
    }
    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(RootedSource(source_meta(
            "tree",
            "field",
            SourceScope::PathScoped,
        )))]
    }
}

/// `$HOME/ws/project` holds the marker; `$HOME/ws/project/foo` is a
/// subdirectory of it, below the entry the provider narrows to.
fn rooted_fixture() -> Fixture {
    let mut reg = ProviderRegistry::new();
    reg.register(Box::new(RootedProvider)).unwrap();
    let f = Fixture::start(reg);
    std::fs::write(f.home.join("ws/project/.root"), "").unwrap();
    f
}

/// `put` defaults to the caller's directory and `get` defaults to the same one,
/// so a record written from a subdirectory of a narrowing provider's scope has
/// to be readable from there. It used to be unreachable: the merged read
/// started at the *narrowed* entry, one or more links above the record.
#[tokio::test]
async fn a_static_record_below_the_provider_entry_is_read_from_the_requested_path() {
    let f = rooted_fixture();
    let root = f.path("ws/project");
    let sub = f.path("ws/project/foo");
    let mut s = f.connect().await;

    assert_eq!(
        data(
            send_recv(&mut s, get_at("r.field", &sub)).await,
            "provider record"
        ),
        serde_json::json!(root),
        "the path-scoped source runs at the marker directory, not the requested one"
    );
    assert!(
        f.cache.get_entry("r", Some(&root)).is_some(),
        "and its entry is keyed there"
    );
    assert!(
        f.cache.get_entry("r", Some(&sub)).is_none(),
        "nothing of the provider's lands at the requested directory"
    );

    let resp = send_recv(&mut s, put_at("r.note", serde_json::json!("kept"), &sub)).await;
    assert!(resp.ok, "put below the provider entry: {:?}", resp.error);

    assert_eq!(
        data(
            send_recv(&mut s, get_at("r.note", &sub)).await,
            "static record below the entry"
        ),
        serde_json::json!("kept"),
        "the chain starts at the requested path, so the record put there is visible"
    );
    assert_eq!(
        data(
            send_recv(&mut s, get_at("r.note:source", &sub)).await,
            "provenance"
        ),
        serde_json::json!("put"),
        "it is a static record, written by `put`"
    );
    assert_eq!(
        data(
            send_recv(&mut s, get_at("r.field", &sub)).await,
            "provider record again"
        ),
        serde_json::json!(root),
        "and the provider's own record still answers from its narrowed entry"
    );

    let above = send_recv(&mut s, get_at("r.note", &root)).await;
    assert!(above.ok, "a miss is ok=true: {:?}", above.error);
    assert!(
        above.data.is_none(),
        "a record below the requested path is never visible from above, got {:?}",
        above.data
    );
}

/// The chain does not stop at the provider's entry: links below it layer like
/// any other, nearest winning.
#[tokio::test]
async fn the_nearest_static_record_wins_across_the_provider_entry() {
    let f = rooted_fixture();
    let root = f.path("ws/project");
    let sub = f.path("ws/project/foo");
    let other = f.path("ws/project/other");
    std::fs::create_dir_all(&other).unwrap();
    let mut s = f.connect().await;

    send_recv(&mut s, put_at("r.note", serde_json::json!("root"), &root)).await;
    send_recv(&mut s, put_at("r.note", serde_json::json!("sub"), &sub)).await;

    assert_eq!(
        data(send_recv(&mut s, get_at("r.note", &sub)).await, "at sub"),
        serde_json::json!("sub"),
        "the nearest record wins"
    );
    assert_eq!(
        data(send_recv(&mut s, get_at("r.note", &root)).await, "at root"),
        serde_json::json!("root"),
        "the record a link down is not visible from the provider entry"
    );
    assert_eq!(
        data(
            send_recv(&mut s, get_at("r.note", &other)).await,
            "at other"
        ),
        serde_json::json!("root"),
        "a sibling of the deeper directory resolves to the entry's record"
    );
}

/// A watch subscribes the chain from the requested path too — otherwise a `put`
/// below the provider's entry changes what the watch resolves to without ever
/// waking it.
#[tokio::test]
async fn a_watch_fires_for_a_static_record_below_the_provider_entry() {
    let f = rooted_fixture();
    let sub = f.path("ws/project/foo");

    let mut w = f.connect().await;
    let mut req = serde_json::json!({"op": "watch", "key": "r.note", "path": sub}).to_string();
    req.push('\n');
    w.write_all(req.as_bytes()).await.unwrap();
    let mut frames = BufReader::new(w);

    let mut line = String::new();
    tokio::time::timeout(Duration::from_millis(2000), frames.read_line(&mut line))
        .await
        .expect("the initial frame must arrive")
        .unwrap();
    let initial: Response = serde_json::from_str(&line).unwrap();
    assert!(
        initial.data.is_none(),
        "nothing is written yet, got {:?}",
        initial.data
    );

    let mut s = f.connect().await;
    let resp = send_recv(&mut s, put_at("r.note", serde_json::json!("kept"), &sub)).await;
    assert!(resp.ok, "put below the provider entry: {:?}", resp.error);

    line.clear();
    tokio::time::timeout(Duration::from_millis(2000), frames.read_line(&mut line))
        .await
        .expect("a frame must arrive for a record put below the provider entry")
        .unwrap();
    let update: Response = serde_json::from_str(&line).unwrap();
    assert_eq!(update.data, Some(serde_json::json!("kept")));
}

/// The merge takes a provider's records from one entry — the one its source
/// resolves to — never from an ancestor that also holds records of the same
/// namespace. Nearest-wins hides such a leak for a field both entries produce,
/// so the field here exists at the outer root and not at the inner one.
#[tokio::test]
async fn an_ancestor_entrys_provider_records_never_leak_into_a_nested_root() {
    let f = rooted_fixture();
    let outer = f.path("ws/project");
    std::fs::write(f.home.join("ws/project/.root"), "outer").unwrap();
    std::fs::create_dir_all(f.home.join("ws/project/inner")).unwrap();
    std::fs::write(f.home.join("ws/project/inner/.root"), "").unwrap();
    let inner = f.path("ws/project/inner");
    let mut s = f.connect().await;

    let at_outer = data(send_recv(&mut s, get_at("r", &outer)).await, "outer read");
    assert_eq!(
        at_outer,
        serde_json::json!({ "field": outer, "tag": "outer" }),
        "the outer root produces `tag`"
    );

    let at_inner = data(send_recv(&mut s, get_at("r", &inner)).await, "inner read");
    assert_eq!(
        at_inner,
        serde_json::json!({ "field": inner }),
        "the inner root's entry is the only one the merge reads provider records from"
    );

    let leaked = send_recv(&mut s, get_at("r.tag", &inner)).await;
    assert!(
        leaked.data.is_none(),
        "a field the inner root never produces must not arrive from the outer entry, got {:?}",
        leaked.data
    );
}

// ── a provider entry above $HOME is off the chain, and still read ────────────

/// The marker sits *above* `$HOME`: `$ROOT/.root` with `$HOME = $ROOT/home`.
/// The walker does not stop at `$HOME`, so the provider's entry is keyed at
/// `$ROOT` — a directory the chain from anywhere below `$HOME` never reaches,
/// because the chain stops at `$HOME` and steps to the global entry.
fn rooted_fixture_above_home() -> Fixture {
    let mut reg = ProviderRegistry::new();
    reg.register(Box::new(RootedProvider)).unwrap();
    let f = Fixture::start_with_home(reg, "home");
    std::fs::write(f.root.join(".root"), "").unwrap();
    f
}

/// Canon `field_resolution.md` reads a path-scoped source's records at the
/// addressed path, wherever the source resolved it to. An entry above `$HOME`
/// is the one case that is not a link of the requested path's chain, so the
/// read has to add it — and a static record put below `$HOME` still layers
/// over it like any other.
#[tokio::test]
async fn a_provider_entry_above_home_is_read_from_a_path_below_home() {
    let f = rooted_fixture_above_home();
    let root = f.root.to_string_lossy().into_owned();
    let sub = f.path("ws/project");
    let mut s = f.connect().await;

    assert_eq!(
        data(
            send_recv(&mut s, get_at("r.field", &sub)).await,
            "provider record"
        ),
        serde_json::json!(root),
        "the source runs at the marker directory above $HOME"
    );
    assert!(
        f.cache.get_entry("r", Some(&root)).is_some(),
        "and its entry is keyed there, above $HOME"
    );
    assert!(
        f.cache.get_entry("r", Some(&sub)).is_none(),
        "nothing of the provider's lands at the requested directory"
    );

    let resp = send_recv(&mut s, put_at("r.note", serde_json::json!("kept"), &sub)).await;
    assert!(resp.ok, "put below $HOME: {:?}", resp.error);
    assert_eq!(
        data(send_recv(&mut s, get_at("r", &sub)).await, "namespace"),
        serde_json::json!({ "field": root, "note": "kept" }),
        "the static record at the requested path layers over the entry above $HOME"
    );
}

/// The watch subscribes the same chain the read walks, so a provider write to
/// an entry above `$HOME` wakes a watch opened below it.
#[tokio::test]
async fn a_watch_below_home_receives_a_provider_write_above_home() {
    let f = rooted_fixture_above_home();
    let root = f.root.to_string_lossy().into_owned();
    let sub = f.path("ws/project");

    let mut w = f.connect().await;
    let mut req = serde_json::json!({"op": "watch", "key": "r.field", "path": sub}).to_string();
    req.push('\n');
    w.write_all(req.as_bytes()).await.unwrap();
    let mut frames = BufReader::new(w);

    let mut line = String::new();
    tokio::time::timeout(Duration::from_millis(2000), frames.read_line(&mut line))
        .await
        .expect("the initial frame must arrive")
        .unwrap();
    let initial: Response = serde_json::from_str(&line).unwrap();
    assert!(
        initial.data.is_none(),
        "no scheduler and nothing executed yet, got {:?}",
        initial.data
    );

    // A cold-miss `get` on another connection is the provider's write: it
    // executes the source and stores its records at `$ROOT`.
    let mut s = f.connect().await;
    assert_eq!(
        data(
            send_recv(&mut s, get_at("r.field", &sub)).await,
            "cold-miss execute"
        ),
        serde_json::json!(root)
    );

    line.clear();
    tokio::time::timeout(Duration::from_millis(2000), frames.read_line(&mut line))
        .await
        .expect("a frame must arrive for the provider's write above $HOME")
        .unwrap();
    let update: Response = serde_json::from_str(&line).unwrap();
    assert_eq!(update.data, Some(serde_json::json!(root)));
}

// ── (f) a nested repo never inherits its parent's records ────────────────────

#[tokio::test]
async fn a_nested_repo_never_reads_the_outer_repos_branch() {
    if !has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let f = Fixture::defaults();
    let outer = GitRepoFixture::new();
    let inner = outer.create_subdir("inner");
    outer.git_in(&inner, &["init", "--initial-branch=innerbr"]);

    let mut s = f.connect().await;

    let outer_branch = data(
        send_recv(&mut s, get_at("git.branch", outer.path_str())).await,
        "outer branch",
    );
    assert_eq!(outer_branch, serde_json::json!("main"));

    let inner_branch = data(
        send_recv(&mut s, get_at("git.branch", inner.to_str().unwrap())).await,
        "inner branch",
    );
    assert_eq!(
        inner_branch,
        serde_json::json!("innerbr"),
        "a path-scoped record is read at its own path — never inherited from an ancestor"
    );
}

// ── a repository at $HOME is served from the global entry ────────────────────

/// `git`, with the host's configuration shut out (see `common::git`).
fn git_at(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `$HOME` is the global path, so a repository rooted there is a path-scoped
/// source addressed at the global entry: its records key the pathless entry and
/// the source runs at `$HOME`. Addressing `$HOME`, addressing nothing, and
/// standing in a plain subdirectory are all the same read of the same entry —
/// there is never a second entry keyed `Some($HOME)`.
#[tokio::test]
async fn a_repository_at_home_is_served_from_the_global_entry() {
    if !has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let f = Fixture::defaults();
    git_at(&f.home, &["init", "--initial-branch=homebr"]);

    let home = f.home.to_string_lossy().into_owned();
    let mut s = f.connect().await;

    assert_eq!(
        data(
            send_recv(&mut s, get_at("git.branch", &home)).await,
            "$HOME"
        ),
        serde_json::json!("homebr"),
        "a path-scoped source addressed at $HOME runs there"
    );
    assert_eq!(
        data(send_recv(&mut s, get("git.branch")).await, "--global"),
        serde_json::json!("homebr"),
        "--global reads the same entry, and the same directory"
    );

    // A plain subdirectory of the repository, not a repository itself: the
    // scope walk lands on $HOME, which is the global entry.
    let sub = f.path("ws/other");
    assert_eq!(
        data(
            send_recv(&mut s, get_at("git.branch", &sub)).await,
            "subdirectory"
        ),
        serde_json::json!("homebr"),
        "a subdirectory resolves to the repository root, which is $HOME"
    );

    let rows = data(
        send_recv(&mut s, serde_json::json!({"op": "status"})).await,
        "status",
    );
    let paths: std::collections::BTreeSet<String> = rows
        .as_array()
        .expect("status returns an array")
        .iter()
        .filter(|row| row["provider"] == "git")
        .map(|row| row["path"].to_string())
        .collect();
    assert_eq!(
        paths,
        std::collections::BTreeSet::from(["null".to_string()]),
        "one git entry, keyed pathless — never a second one at Some($HOME)"
    );
}

/// The bare-namespace read follows the field read: a cold cache, a repository
/// at `$HOME`, and `get git --global` answers with the object rather than a
/// miss, because a path-scoped source addressed at the global entry runs at
/// `$HOME` like any other directory.
#[tokio::test]
async fn a_bare_global_read_of_a_home_repository_is_not_a_miss() {
    if !has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let f = Fixture::defaults();
    git_at(&f.home, &["init", "--initial-branch=homebr"]);

    let mut s = f.connect().await;
    let whole = data(send_recv(&mut s, get("git")).await, "bare --global");
    assert_eq!(
        whole["branch"],
        serde_json::json!("homebr"),
        "the whole-namespace read carries the repository's fields"
    );
    assert_eq!(
        data(send_recv(&mut s, get("git.branch")).await, "field --global"),
        serde_json::json!("homebr"),
        "and the field read agrees with it"
    );
}

/// The mirror image, and the reason every git source guards on being inside a
/// work tree: with no repository at `$HOME` the same bare read is a clean miss,
/// not an object of zeroes from a source that reports defaults for a directory
/// it knows nothing about.
#[tokio::test]
async fn a_bare_global_read_without_a_home_repository_is_a_miss() {
    if !has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let f = Fixture::defaults();
    let mut s = f.connect().await;

    let resp = send_recv(&mut s, get("git")).await;
    assert!(resp.ok, "a miss is ok=true: {:?}", resp.error);
    assert!(
        resp.data.is_none(),
        "no repository at $HOME means nothing to report, got {:?}",
        resp.data
    );

    let resp = send_recv(&mut s, get("git.lines_added")).await;
    assert!(resp.ok, "a miss is ok=true: {:?}", resp.error);
    assert!(
        resp.data.is_none(),
        "git.lines_added must not answer 0 outside a work tree, got {:?}",
        resp.data
    );
}

/// A watch from a subdirectory of a repository rooted at `$HOME` subscribes to
/// the global entry, which is on its chain, and its initial frame is the
/// chain-resolved value.
#[tokio::test]
async fn watch_from_a_subdirectory_of_a_home_repository_reads_the_global_entry() {
    if !has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let f = Fixture::defaults();
    git_at(&f.home, &["init", "--initial-branch=homebr"]);

    let sub = f.path("ws/other");
    let mut w = f.connect().await;
    let mut req = serde_json::json!({"op": "watch", "key": "git.branch", "path": sub}).to_string();
    req.push('\n');
    w.write_all(req.as_bytes()).await.unwrap();
    let mut frames = BufReader::new(w);

    let mut line = String::new();
    tokio::time::timeout(Duration::from_millis(2000), frames.read_line(&mut line))
        .await
        .expect("the initial frame must arrive")
        .unwrap();
    let initial: Response = serde_json::from_str(&line).unwrap();
    assert_eq!(
        initial.data,
        Some(serde_json::json!("homebr")),
        "the initial frame resolves through the chain to the global entry"
    );
}

// ── (h) a watch fires for every entry on the chain ───────────────────────────

#[tokio::test]
async fn watch_follows_the_whole_chain() {
    let f = Fixture::defaults();
    let mut s = f.connect().await;
    send_recv(&mut s, put("my", serde_json::json!({"b": 1}))).await;
    send_recv(
        &mut s,
        put_at("my", serde_json::json!({"b": 2}), &f.path("ws/project")),
    )
    .await;

    let deep = f.path("ws/project/foo/bar/baz");
    let mut w = f.connect().await;
    let mut req = serde_json::json!({"op": "watch", "key": "my.b", "path": deep}).to_string();
    req.push('\n');
    w.write_all(req.as_bytes()).await.unwrap();
    let mut frames = BufReader::new(w);

    let mut line = String::new();
    tokio::time::timeout(Duration::from_millis(2000), frames.read_line(&mut line))
        .await
        .expect("the initial frame must arrive")
        .unwrap();
    let initial: Response = serde_json::from_str(&line).unwrap();
    assert_eq!(
        initial.data,
        Some(serde_json::json!(2)),
        "the initial frame is the chain-resolved value"
    );

    // A global write the project record shadows changes nothing the watcher can
    // see, so no frame is emitted.
    // The cancellation here is safe where the one in the refresh test was not:
    // `frames` is a one-way watch stream this test never writes to, so a
    // cancelled read strands no response — only, if a frame did arrive, a
    // partial line, which the emptiness check below turns into a legible
    // failure rather than a parse error on the next read.
    send_recv(&mut s, put("my", serde_json::json!({"b": 9}))).await;
    line.clear();
    let shadowed = tokio::time::timeout(Duration::from_millis(250), frames.read_line(&mut line));
    let timed_out = shadowed.await.is_err();
    assert!(
        timed_out && line.is_empty(),
        "a shadowed change must not produce a frame, got {line:?}"
    );

    // Clearing the project record uncovers the global one — the watch is
    // subscribed to that entry too, so it fires.
    let clear = serde_json::json!({
        "op": "put", "key": "my.b", "data": null, "path": f.path("ws/project"),
    });
    send_recv(&mut s, clear).await;
    line.clear();
    tokio::time::timeout(Duration::from_millis(2000), frames.read_line(&mut line))
        .await
        .expect("a frame must arrive when the nearest record is removed")
        .unwrap();
    let uncovered: Response = serde_json::from_str(&line).unwrap();
    assert_eq!(
        uncovered.data,
        Some(serde_json::json!(9)),
        "the uncovered global value is streamed"
    );
}

// ── refresh reaches the scheduler's execute path ─────────────────────────────

/// A real daemon — scheduler included, so `refresh` reaches
/// `Scheduler::execute_source` rather than the server's scheduler-less no-op —
/// with `$HOME` a fresh temp root.
async fn daemon_at_a_temp_home() -> (TempDir, TempDir, PathBuf, PathBuf) {
    let home_dir = TempDir::new().unwrap();
    let home = home_dir.path().canonicalize().unwrap();
    // SAFETY: nextest gives every test its own process.
    unsafe { std::env::set_var("HOME", &home) };

    let sock_dir = TempDir::new().unwrap();
    let sock = sock_dir.path().join("test.sock");
    let (_handle, ready) = beachcomber::daemon::start_in_process_polling(
        sock.clone(),
        beachcomber::config::Config::default(),
    );
    ready.await.expect("daemon ready");
    (home_dir, sock_dir, home, sock)
}

/// The cache rows `status` reports for `git`, as `(path, field)` pairs. Reads
/// the cache without querying it, so it observes what `refresh` wrote rather
/// than triggering an inline execute of its own.
async fn git_rows(stream: &mut UnixStream) -> Vec<(String, String)> {
    let rows = data(
        send_recv(stream, serde_json::json!({"op": "status"})).await,
        "status",
    );
    rows.as_array()
        .expect("status returns an array")
        .iter()
        .filter(|row| row["provider"] == "git")
        .map(|row| (row["path"].to_string(), row["field"].to_string()))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_at_the_global_entry_runs_the_home_repository() {
    if !has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let (_home_dir, _sock_dir, home, sock) = daemon_at_a_temp_home().await;
    git_at(&home, &["init", "--initial-branch=homebr"]);

    let mut s = UnixStream::connect(&sock).await.unwrap();
    let resp = send_recv(&mut s, serde_json::json!({"op": "refresh", "key": "git"})).await;
    assert!(resp.ok, "refresh: {:?}", resp.error);

    // Every source fans out from one refresh and each writes when it finishes,
    // so wait for the last one this test names rather than the first to land.
    common::poll::poll_until_async(
        "git rows after a global refresh",
        "rows for both the head and the diff source",
        Duration::from_secs(10),
        Duration::from_millis(20),
        async || git_rows(&mut s).await,
        |rows| {
            let has = |f: &str| rows.iter().any(|(_, field)| field == f);
            has("\"branch\"") && has("\"lines_added\"")
        },
    )
    .await;

    let whole = data(send_recv(&mut s, get("git")).await, "--global");
    assert_eq!(whole["branch"], serde_json::json!("homebr"));
    assert!(
        whole.get("lines_added").is_some(),
        "the diff source ran too, got {whole}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_at_the_global_entry_writes_nothing_without_a_home_repository() {
    if !has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let (_home_dir, _sock_dir, _home, sock) = daemon_at_a_temp_home().await;

    let mut s = UnixStream::connect(&sock).await.unwrap();
    let resp = send_recv(&mut s, serde_json::json!({"op": "refresh", "key": "git"})).await;
    assert!(resp.ok, "refresh: {:?}", resp.error);

    // The refresh executes asynchronously, so give it a window to produce a row
    // and require that none appears. The inner poll's own deadline is longer
    // than the outer one, so the outer timeout is what ends this.
    //
    // The poll gets a connection of its own, which is then dropped: the timeout
    // cancels `poll_until_async` wherever it happens to be, and if that is
    // between a request's `write_all` and its `read_line` the response is left
    // unread on the wire for whatever reads that connection next. On a shared
    // connection the assertions below would then read the abandoned `status`
    // reply instead of their own.
    let wrote = {
        let mut probe = UnixStream::connect(&sock).await.unwrap();
        tokio::time::timeout(
            Duration::from_millis(400),
            common::poll::poll_until_async(
                "git rows after a global refresh",
                "any row",
                Duration::from_secs(30),
                Duration::from_millis(20),
                async || git_rows(&mut probe).await,
                |rows| !rows.is_empty(),
            ),
        )
        .await
    };
    assert!(
        wrote.is_err(),
        "a refresh outside a work tree must write nothing, got {wrote:?}"
    );

    let mut s = UnixStream::connect(&sock).await.unwrap();
    let resp = send_recv(&mut s, get("git")).await;
    assert!(resp.data.is_none(), "still a miss, got {:?}", resp.data);
    let resp = send_recv(&mut s, get("git.lines_added")).await;
    assert!(
        resp.data.is_none(),
        "git.lines_added must not answer 0, got {:?}",
        resp.data
    );
}

// ── (i)+(j) the CLI carries a path on every request ──────────────────────────

fn comb(f: &Fixture, cwd: &Path, cfg: &Path) -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("comb").unwrap();
    cmd.env("BEACHCOMBER_SOCKET", &f.sock)
        .env("RUST_LOG", "error")
        .env("HOME", &f.home)
        .env("XDG_CONFIG_HOME", cfg)
        .current_dir(cwd);
    cmd
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_put_and_get_default_to_the_cwd() {
    let f = Fixture::defaults();
    let cwd = PathBuf::from(f.path("ws/project"));
    let cfg = f.home.join("cfg");

    let out = tokio::task::spawn_blocking(move || {
        comb(&f, &cwd, &cfg)
            .args(["put", "my", r#"{"a":1}"#])
            .assert()
            .success();
        let got = comb(&f, &cwd, &cfg)
            .args(["get", "my.a"])
            .assert()
            .success();
        let at_cwd = String::from_utf8(got.get_output().stdout.clone()).unwrap();

        let global = comb(&f, &cwd, &cfg)
            .args(["get", "--global", "my.a"])
            .assert()
            .success();
        let at_global = String::from_utf8(global.get_output().stdout.clone()).unwrap();
        (at_cwd, at_global)
    })
    .await
    .unwrap();

    assert_eq!(out.0.trim(), "1", "put and get share the cwd default");
    assert_eq!(
        out.1.trim(),
        "",
        "--global reads $HOME, where nothing was written"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_rejects_global_together_with_path() {
    let f = Fixture::defaults();
    let cwd = PathBuf::from(f.path("ws/project"));
    let cfg = f.home.join("cfg");
    let arg = f.path("ws/other");

    tokio::task::spawn_blocking(move || {
        comb(&f, &cwd, &cfg)
            .args(["get", "--global", "--path", &arg, "my.a"])
            .assert()
            .code(2);
        comb(&f, &cwd, &cfg)
            .args(["put", "--global", "--path", &arg, "my", r#"{"a":1}"#])
            .assert()
            .code(2);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_carries_the_cwd_for_a_plain_get_from_a_subdirectory() {
    if !has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let f = Fixture::defaults();
    let repo = GitRepoFixture::new();
    let sub = repo.create_subdir("src/lib");
    let cfg = f.home.join("cfg");

    let out = tokio::task::spawn_blocking(move || {
        let got = comb(&f, &sub, &cfg)
            .args(["get", "git.branch"])
            .assert()
            .success();
        String::from_utf8(got.get_output().stdout.clone()).unwrap()
    })
    .await
    .unwrap();

    assert_eq!(
        out.trim(),
        "main",
        "a plain get with no --path carries the cwd, so the repo root resolves"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_carries_the_cwd_for_a_templates_dependency_fetch() {
    let f = Fixture::defaults();
    let project = PathBuf::from(f.path("ws/project"));
    let deep = PathBuf::from(f.path("ws/project/foo/bar/baz"));
    let cfg = f.home.join("cfg");

    let out = tokio::task::spawn_blocking(move || {
        // Both the template and the value it reads live at the project; the
        // read is from a directory beneath it.
        comb(&f, &project, &cfg)
            .args(["put", "myvirt:template.w", r#""{{ cache.seed.v }}""#])
            .assert()
            .success();
        comb(&f, &project, &cfg)
            .args(["put", "seed", r#"{"v":"chain-ok"}"#])
            .assert()
            .success();
        let got = comb(&f, &deep, &cfg)
            .args(["get", "myvirt.w"])
            .assert()
            .success();
        String::from_utf8(got.get_output().stdout.clone()).unwrap()
    })
    .await
    .unwrap();

    assert_eq!(
        out.trim(),
        "chain-ok",
        "the template and its dependency fetch carry the cwd, so the chain finds both"
    );
}
