//! `$HOME` is the global path, and a read resolves static records up the
//! directory chain (canon `field_resolution.md` §"Path resolution").
//!
//! The chain for a path `P` is `P`, each ancestor up to `$HOME` (or to `/` when
//! `P` is outside `$HOME`), then the global entry. Static records — those whose
//! source is `put` or `config:<file>` — are collected from every entry on the
//! chain, the nearest winning. Provider-produced records keep their Source's
//! declared scope: a `Global` source is read at the global entry, a
//! `PathScoped` one at the addressed path; neither walks the chain.
//!
//! Each test owns its `$HOME`: nextest runs every test in its own process, so
//! `set_var("HOME", …)` here is process-local.

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
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

// ── fixture ──────────────────────────────────────────────────────────────────

struct Fixture {
    _sock_dir: TempDir,
    _home: TempDir,
    sock: PathBuf,
    home: PathBuf,
    cache: Arc<Cache>,
}

impl Fixture {
    /// `$HOME` is a fresh temp root holding `ws/project/foo/bar/baz` and
    /// `ws/other`. The server runs in-process on a unix socket.
    fn start(registry: ProviderRegistry) -> Fixture {
        let home_dir = TempDir::new().unwrap();
        let home = home_dir.path().canonicalize().unwrap();
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
            _home: home_dir,
            sock,
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
    assert!(
        f.cache.has_entry("my", None),
        "the record is keyed pathless, not under $HOME"
    );
    assert!(
        !f.cache.has_entry("my", Some(&home)),
        "no separate $HOME-keyed entry exists"
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

/// A provider record and a static record inside the same object node: the read
/// stitches them together instead of letting one map answer alone.
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
    let resp = send_recv(&mut s, put("p.tools.x", serde_json::json!(1))).await;
    assert!(resp.ok, "put beside a provider record: {:?}", resp.error);

    assert_eq!(
        data(
            send_recv(&mut s, get_at("p.tools", &project)).await,
            "interior object"
        ),
        serde_json::json!({"x": 1, "y": 2}),
        "the static and provider halves of one object node both show up"
    );
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
    frames.read_line(&mut line).await.unwrap();
    let initial: Response = serde_json::from_str(&line).unwrap();
    assert_eq!(
        initial.data,
        Some(serde_json::json!(2)),
        "the initial frame is the chain-resolved value"
    );

    // A global write the project record shadows changes nothing the watcher can
    // see, so no frame is emitted.
    send_recv(&mut s, put("my", serde_json::json!({"b": 9}))).await;
    line.clear();
    let shadowed = tokio::time::timeout(Duration::from_millis(250), frames.read_line(&mut line));
    assert!(
        shadowed.await.is_err(),
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
async fn cli_carries_the_cwd_for_a_virtual_fields_dependency_fetch() {
    let f = Fixture::defaults();
    let project = PathBuf::from(f.path("ws/project"));
    let deep = PathBuf::from(f.path("ws/project/foo/bar/baz"));
    let cfg = f.home.join("cfg");
    std::fs::create_dir_all(cfg.join("beachcomber")).unwrap();
    std::fs::write(
        cfg.join("beachcomber").join("config.toml"),
        "[providers.myvirt.virtual]\nw = \"{{ cache.seed.v }}\"\n",
    )
    .unwrap();

    let project_arg = project.to_string_lossy().into_owned();
    let out = tokio::task::spawn_blocking(move || {
        comb(&f, &project, &cfg)
            .args(["put", "seed", r#"{"v":"chain-ok"}"#, "--path", &project_arg])
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
        "the daemon dependency fetch carries the cwd, so the chain finds the seed"
    );
}
