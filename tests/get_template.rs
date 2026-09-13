//! The template half of `get`, and per-record freshness (spec §3.4).
//!
//! Every `get` on a non-facet key does two lookups: the data at `rest` in the
//! namespace's merged tree, where the provider's records live, and the node at
//! `rest` in the merged tree of the `template` facet (`p:template`), read from
//! the requested directory because a template is a static record. Whichever
//! exist come back — `data` with its own `age_ms`/`stale`, `template` (plus
//! `rest` when the walk stopped on a string with segments left over) with none.
//!
//! Freshness is per record: a leaf reports its own record's source entry, an
//! object reports the largest age among the records under it and is stale if
//! any of them is. A response with no `data` carries neither field.
//!
//! Raw-socket style, mirroring `tests/put_node.rs` and `tests/facets.rs`.
//! Templates are written with `put` here — seeding them from config is Task 6.

mod common;

use beachcomber::cache::Cache;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use beachcomber::server::Server;
use common::git::{GitRepoFixture, has_git};
use std::sync::Arc;
use std::time::Duration;
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

/// Send one request, return the response as raw wire JSON — the exact object
/// the daemon emitted, so a test can pin every field it does and does not carry.
async fn send_raw(stream: &mut UnixStream, request: &serde_json::Value) -> serde_json::Value {
    let mut line = request.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes()).await.unwrap();
    let mut reader = BufReader::new(&mut *stream);
    let mut resp_line = String::new();
    reader.read_line(&mut resp_line).await.unwrap();
    serde_json::from_str(&resp_line).unwrap()
}

/// The response minus `age_ms`, which is a wall-clock reading and cannot be
/// pinned. Its presence is asserted separately where it matters.
fn without_age(mut resp: serde_json::Value) -> serde_json::Value {
    resp.as_object_mut()
        .expect("response is an object")
        .remove("age_ms");
    resp
}

async fn get(stream: &mut UnixStream, key: &str) -> serde_json::Value {
    send_raw(stream, &serde_json::json!({"op": "get", "key": key})).await
}

async fn put(stream: &mut UnixStream, key: &str, data: serde_json::Value) {
    let r = send_raw(
        stream,
        &serde_json::json!({"op": "put", "key": key, "data": data}),
    )
    .await;
    assert_eq!(r["ok"], true, "put {key} failed: {r}");
}

/// `scanline` as spec §3.4 draws it: one literal record, and a template facet
/// holding a value expression, a group of them, and one that renders an object.
async fn seed_scanline(stream: &mut UnixStream) {
    put(stream, "scanline", serde_json::json!({"lit": 1})).await;
    seed_scanline_template(stream).await;
}

async fn seed_scanline_template(stream: &mut UnixStream) {
    put(
        stream,
        "scanline:template",
        serde_json::json!({
            "theme": "{{ env.SCANLINE_THEME }}",
            "widgets": {
                "foo": "{{ env.SCANLINE_FOO }}",
                "bar": "{{ env.SCANLINE_BAR }}"
            },
            "stats": "{{ {'cpu': 40, 'mem': 12} }}"
        }),
    )
    .await;
}

fn scanline_template_tree() -> serde_json::Value {
    serde_json::json!({
        "theme": "{{ env.SCANLINE_THEME }}",
        "widgets": {
            "foo": "{{ env.SCANLINE_FOO }}",
            "bar": "{{ env.SCANLINE_BAR }}"
        },
        "stats": "{{ {'cpu': 40, 'mem': 12} }}"
    })
}

// ── (a) the spec's example responses, byte for byte modulo age_ms ───────────

#[tokio::test]
async fn get_a_template_leaf_returns_the_expression_and_nothing_else() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();
    seed_scanline(&mut s).await;

    assert_eq!(
        without_age(get(&mut s, "scanline.widgets.foo").await),
        serde_json::json!({"ok": true, "template": "{{ env.SCANLINE_FOO }}"})
    );

    handle.abort();
}

#[tokio::test]
async fn get_a_template_group_returns_the_whole_object() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();
    seed_scanline(&mut s).await;

    assert_eq!(
        without_age(get(&mut s, "scanline.widgets").await),
        serde_json::json!({
            "ok": true,
            "template": {
                "foo": "{{ env.SCANLINE_FOO }}",
                "bar": "{{ env.SCANLINE_BAR }}"
            }
        })
    );

    handle.abort();
}

/// A bare `get p` carries both halves: the namespace's data and the whole
/// template tree. The data half is a static record, so it reports its own
/// freshness like any other record.
#[tokio::test]
async fn get_a_namespace_returns_data_and_the_whole_template_tree() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();
    seed_scanline(&mut s).await;

    let resp = get(&mut s, "scanline").await;
    assert!(
        resp["age_ms"].is_number(),
        "a data hit carries age_ms: {resp}"
    );
    assert_eq!(
        without_age(resp),
        serde_json::json!({
            "ok": true,
            "data": {"lit": 1},
            "stale": false,
            "template": scanline_template_tree(),
        })
    );

    handle.abort();
}

/// The walk lands on a string with `cpu` left over: the string is the template
/// and the remainder comes back as `rest` for the client to apply to whatever
/// the expression renders.
#[tokio::test]
async fn get_below_a_template_string_returns_it_with_the_leftover_rest() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();
    seed_scanline(&mut s).await;

    assert_eq!(
        without_age(get(&mut s, "scanline.stats.cpu").await),
        serde_json::json!({
            "ok": true,
            "template": "{{ {'cpu': 40, 'mem': 12} }}",
            "rest": "cpu"
        })
    );

    handle.abort();
}

/// A namespace with no template facet answers exactly as it did before: data,
/// its freshness, and no template half at all.
#[tokio::test]
async fn get_a_cached_field_without_a_template_is_unchanged() {
    if !has_git() {
        return;
    }
    let repo = GitRepoFixture::new();
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    let resp = send_raw(
        &mut s,
        &serde_json::json!({"op":"get","key":"git.branch","path": repo.path_str()}),
    )
    .await;
    assert!(
        resp["age_ms"].is_number(),
        "a data hit carries age_ms: {resp}"
    );
    assert_eq!(
        without_age(resp),
        serde_json::json!({"ok": true, "data": "main", "stale": false})
    );

    handle.abort();
}

/// (h) A template at a cached field's path: both halves come back, the real
/// branch as `data` and the expression as `template`.
#[tokio::test]
async fn get_a_cached_field_with_a_template_returns_both_halves() {
    if !has_git() {
        return;
    }
    let repo = GitRepoFixture::new();
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    put(
        &mut s,
        "git:template.branch",
        serde_json::json!("{{ env.GIT_BRANCH or cache.git.branch }}"),
    )
    .await;

    let resp = send_raw(
        &mut s,
        &serde_json::json!({"op":"get","key":"git.branch","path": repo.path_str()}),
    )
    .await;
    assert!(
        resp["age_ms"].is_number(),
        "a data hit carries age_ms: {resp}"
    );
    assert_eq!(
        without_age(resp),
        serde_json::json!({
            "ok": true,
            "data": "main",
            "stale": false,
            "template": "{{ env.GIT_BRANCH or cache.git.branch }}"
        })
    );

    handle.abort();
}

// ── (b) a facet key never carries a template half ───────────────────────────

#[tokio::test]
async fn get_on_the_facet_itself_returns_the_template_as_data() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();
    seed_scanline(&mut s).await;

    assert_eq!(
        without_age(get(&mut s, "scanline:template.widgets.foo").await),
        serde_json::json!({
            "ok": true,
            "data": "{{ env.SCANLINE_FOO }}",
            "stale": false
        })
    );

    assert_eq!(
        without_age(get(&mut s, "scanline:template").await),
        serde_json::json!({
            "ok": true,
            "data": scanline_template_tree(),
            "stale": false
        })
    );

    handle.abort();
}

// ── (c) the walk's remaining cases ──────────────────────────────────────────

#[tokio::test]
async fn leftover_segments_are_joined_back_into_rest() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();
    seed_scanline(&mut s).await;

    assert_eq!(
        without_age(get(&mut s, "scanline.stats.cpu.deep").await),
        serde_json::json!({
            "ok": true,
            "template": "{{ {'cpu': 40, 'mem': 12} }}",
            "rest": "cpu.deep"
        })
    );
    assert_eq!(
        without_age(get(&mut s, "scanline.widgets.foo.x").await),
        serde_json::json!({
            "ok": true,
            "template": "{{ env.SCANLINE_FOO }}",
            "rest": "x"
        })
    );

    handle.abort();
}

#[tokio::test]
async fn a_template_miss_attaches_nothing() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();
    seed_scanline(&mut s).await;

    assert_eq!(
        without_age(get(&mut s, "scanline.nope").await),
        serde_json::json!({"ok": true})
    );

    handle.abort();
}

/// Addressed directly, a non-string scalar is not a template — only a group or
/// a value expression is. Contrast the pass-through below.
#[tokio::test]
async fn a_non_string_leaf_at_the_address_attaches_nothing() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();
    put(&mut s, "p:template.n", serde_json::json!(7)).await;

    assert_eq!(
        without_age(get(&mut s, "p.n").await),
        serde_json::json!({"ok": true})
    );

    handle.abort();
}

/// A non-string leaf inside a template group is passed through untouched — the
/// client is what decides an expression is not one.
#[tokio::test]
async fn a_non_string_leaf_inside_a_group_is_passed_through() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();
    seed_scanline(&mut s).await;
    put(&mut s, "scanline:template.widgets.n", serde_json::json!(7)).await;

    assert_eq!(
        without_age(get(&mut s, "scanline.widgets").await),
        serde_json::json!({
            "ok": true,
            "template": {
                "foo": "{{ env.SCANLINE_FOO }}",
                "bar": "{{ env.SCANLINE_BAR }}",
                "n": 7
            }
        })
    );

    handle.abort();
}

// ── the template half decides what an unresolvable sub-path means ───────────

/// `box.widgets` is an object with no `foo` in it, so the data half calls the
/// address a nested-not-found. A template answers there, so the key is not
/// unknown at all: the loud error only wins when both halves are empty.
async fn seed_box(s: &mut UnixStream) {
    put(s, "box", serde_json::json!({"widgets": {"bar": 1}})).await;
    put(
        s,
        "box:template",
        serde_json::json!({"widgets": {"foo": "{{ env.FOO }}"}}),
    )
    .await;
}

#[tokio::test]
async fn a_template_answers_where_the_data_half_has_nothing() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();
    seed_box(&mut s).await;

    assert_eq!(
        without_age(get(&mut s, "box.widgets.foo").await),
        serde_json::json!({"ok": true, "template": "{{ env.FOO }}"})
    );

    // Both halves at the group.
    let both = get(&mut s, "box.widgets").await;
    assert!(
        both["age_ms"].is_number(),
        "a data hit carries age_ms: {both}"
    );
    assert_eq!(
        without_age(both),
        serde_json::json!({
            "ok": true,
            "data": {"bar": 1},
            "stale": false,
            "template": {"foo": "{{ env.FOO }}"}
        })
    );

    // Data only, where the template tree has nothing.
    let data_only = get(&mut s, "box.widgets.bar").await;
    assert_eq!(
        without_age(data_only),
        serde_json::json!({"ok": true, "data": 1, "stale": false})
    );

    // A miss where neither half answers — an address into an existing object
    // that has no such key misses exactly as a missing top-level field does.
    let neither = get(&mut s, "box.widgets.nope").await;
    assert_eq!(
        without_age(neither),
        serde_json::json!({"ok": true}),
        "a sub-path nothing answers is a miss, not an error"
    );

    handle.abort();
}

#[tokio::test]
async fn a_watch_frame_carries_the_template_where_the_data_half_has_nothing() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut s = UnixStream::connect(&sock).await.unwrap();
    seed_box(&mut s).await;

    let mut w = UnixStream::connect(&sock).await.unwrap();
    w.write_all(b"{\"op\":\"watch\",\"key\":\"box.widgets.foo\"}\n")
        .await
        .unwrap();
    let mut reader = BufReader::new(&mut w);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let frame: serde_json::Value = serde_json::from_str(line.trim()).unwrap();

    assert_eq!(
        without_age(frame),
        serde_json::json!({"ok": true, "template": "{{ env.FOO }}"})
    );

    handle.abort();
}

// ── templates resolve from the requested directory ──────────────────────────

/// A template is a static record, so it resolves up the chain from where the
/// caller is standing. `hostname` has only `Global` sources, so `resolve_path`
/// collapses the data half's address to the global entry — the template half
/// must not follow it there.
#[tokio::test]
async fn a_template_resolves_from_the_requested_directory_not_the_scope() {
    let dir = TempDir::new().unwrap();
    let dir = dir.path().to_str().unwrap();
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    let r = send_raw(
        &mut s,
        &serde_json::json!({
            "op": "put",
            "key": "hostname:template",
            "data": {"name": "{{ local }}"},
            "path": dir,
        }),
    )
    .await;
    assert_eq!(r["ok"], true, "put: {r}");

    let here = send_raw(
        &mut s,
        &serde_json::json!({"op":"get","key":"hostname.name","path": dir}),
    )
    .await;
    assert_eq!(
        here["template"], "{{ local }}",
        "the template at the requested directory: {here}"
    );

    let global = get(&mut s, "hostname.name").await;
    assert!(
        global.get("template").is_none(),
        "the global read is not under that directory: {global}"
    );

    handle.abort();
}

/// git narrows the data half's address to the repository root. A template put
/// in a subdirectory belongs to that subdirectory, not to the whole repo.
#[tokio::test]
async fn a_template_below_the_repo_root_stays_below_it() {
    if !has_git() {
        return;
    }
    let repo = GitRepoFixture::new();
    let sub = repo.create_subdir("sub");
    let sub = sub.to_str().unwrap();
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    let r = send_raw(
        &mut s,
        &serde_json::json!({
            "op": "put",
            "key": "git:template",
            "data": {"branch": "{{ sub }}"},
            "path": sub,
        }),
    )
    .await;
    assert_eq!(r["ok"], true, "put: {r}");

    let below = send_raw(
        &mut s,
        &serde_json::json!({"op":"get","key":"git.branch","path": sub}),
    )
    .await;
    assert_eq!(
        below["template"], "{{ sub }}",
        "the subdirectory's own template: {below}"
    );
    assert_eq!(below["data"], "main", "the data half is git's, at the root");

    let at_root = send_raw(
        &mut s,
        &serde_json::json!({"op":"get","key":"git.branch","path": repo.path_str()}),
    )
    .await;
    assert!(
        at_root.get("template").is_none(),
        "the root is not under the subdirectory: {at_root}"
    );

    handle.abort();
}

/// Templates layer up the chain like any other static record: nearest wins,
/// and a directory that is not below the near one still sees the far one.
#[tokio::test]
async fn a_nearer_template_shadows_a_farther_one() {
    let parent = TempDir::new().unwrap();
    let sub = parent.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let (parent, sub) = (
        parent.path().to_str().unwrap().to_string(),
        sub.to_str().unwrap().to_string(),
    );
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    put(&mut s, "p:template.x", serde_json::json!("{{ far }}")).await;
    let r = send_raw(
        &mut s,
        &serde_json::json!({
            "op": "put", "key": "p:template.x", "data": "{{ near }}", "path": sub,
        }),
    )
    .await;
    assert_eq!(r["ok"], true, "put at the subdirectory: {r}");

    let near = send_raw(
        &mut s,
        &serde_json::json!({"op":"get","key":"p.x","path": sub}),
    )
    .await;
    assert_eq!(near["template"], "{{ near }}", "got {near}");

    let far = send_raw(
        &mut s,
        &serde_json::json!({"op":"get","key":"p.x","path": parent}),
    )
    .await;
    assert_eq!(
        far["template"], "{{ far }}",
        "the parent is not below the subdirectory: {far}"
    );

    handle.abort();
}

// ── (d) a namespace with templates only ─────────────────────────────────────

/// `scanline` is not a registered provider and holds no data at all, only a
/// template facet. That is a hit, not `unknown provider`.
#[tokio::test]
async fn a_namespace_with_only_templates_is_not_unknown() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();
    seed_scanline_template(&mut s).await;

    assert_eq!(
        without_age(get(&mut s, "scanline.widgets.foo").await),
        serde_json::json!({"ok": true, "template": "{{ env.SCANLINE_FOO }}"})
    );
    assert_eq!(
        without_age(get(&mut s, "scanline").await),
        serde_json::json!({"ok": true, "template": scanline_template_tree()})
    );

    handle.abort();
}

#[tokio::test]
async fn a_namespace_with_neither_data_nor_templates_is_still_unknown() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    let resp = get(&mut s, "definitely_nonexistent.field").await;
    assert_eq!(resp["ok"], false, "got {resp}");
    assert_eq!(resp["error"], "unknown provider: definitely_nonexistent");

    handle.abort();
}

// ── (e) freshness through the socket ────────────────────────────────────────

/// A template-only response has no data, so it reports no freshness either —
/// and `:age` on it is `null`, not a fabricated zero.
#[tokio::test]
async fn a_template_only_response_carries_no_freshness() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();
    seed_scanline_template(&mut s).await;

    let resp = get(&mut s, "scanline.widgets.foo").await;
    assert!(
        resp.get("age_ms").is_none() && resp.get("stale").is_none(),
        "a response with no data carries neither age_ms nor stale: {resp}"
    );

    // A metadata suffix asks one question about the data half, and is answered
    // with that alone — never a template.
    let age = get(&mut s, "scanline.widgets.foo:age").await;
    assert_eq!(age["data"], serde_json::Value::Null, "got {age}");
    assert!(
        age.get("template").is_none(),
        "a metadata-suffix response carries no template half: {age}"
    );
    let stale = get(&mut s, "scanline.widgets.foo:stale").await;
    assert_eq!(stale["data"], serde_json::Value::Null, "got {stale}");

    handle.abort();
}

#[tokio::test]
async fn age_of_a_warm_field_is_a_number() {
    if !has_git() {
        return;
    }
    let repo = GitRepoFixture::new();
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    let resp = send_raw(
        &mut s,
        &serde_json::json!({"op":"get","key":"git.branch:age","path": repo.path_str()}),
    )
    .await;
    assert!(
        resp["data"].is_number(),
        "a warm field's :age is a number: {resp}"
    );

    handle.abort();
}

// ── (f) the socket twin of the canon per-source field freshness pin ─────────

fn twin_meta(name: &str, field: &str) -> SourceMetadata {
    SourceMetadata {
        name: name.into(),
        fields: vec![FieldSchema {
            name: field.into(),
            field_type: FieldType::Int,
        }],
        scope: SourceScope::Global,
        invalidation: InvalidationStrategy::Poll { interval_secs: 600 },
        keep_alive: KeepAlive::Polls(2),
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 30,
        },
        fsevents_reinstate: false,
    }
}

struct TwinSource(SourceMetadata);

impl Source for TwinSource {
    fn metadata(&self) -> &SourceMetadata {
        &self.0
    }
    fn execute(&self, _path: Option<&str>) -> SourceResult {
        SourceResult::new()
    }
}

/// Two Sources in one cache entry, each owning one field.
struct TwinProvider;

impl Provider for TwinProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "twin".to_string(),
            sources: vec![twin_meta("alpha", "a"), twin_meta("beta", "b")],
        }
    }
    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![
            Box::new(TwinSource(twin_meta("alpha", "a"))),
            Box::new(TwinSource(twin_meta("beta", "b"))),
        ]
    }
}

/// Canon §"Field freshness", through `get`: a field's age is its *owning*
/// Source's last refresh, not the entry's oldest. `alpha` refreshed 10ms before
/// `beta`, so `twin.a` is 10ms old and `twin.b` is brand new — and the whole
/// namespace reports the older of the two.
///
/// The twin of `field_freshness_reflects_owning_source_last_refresh` in
/// `tests/canon_provider_source.rs`, which pins the same rule through
/// `Cache::get_field`. This one survives that call being retired.
#[tokio::test(start_paused = true)]
async fn field_age_through_get_is_the_owning_sources_age() {
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(TwinProvider)).unwrap();
    let (_tmp, sock, cache, registry, watchers) = setup_with_registry(registry);

    cache.put_source(
        "twin",
        None,
        "alpha",
        [("a".to_string(), Value::Int(1))].into_iter().collect(),
        Some(600),
    );
    tokio::time::advance(Duration::from_millis(10)).await;
    cache.put_source(
        "twin",
        None,
        "beta",
        [("b".to_string(), Value::Int(2))].into_iter().collect(),
        Some(600),
    );

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();
    let mut s = UnixStream::connect(&sock).await.unwrap();

    let a = get(&mut s, "twin.a").await;
    let b = get(&mut s, "twin.b").await;
    assert_eq!(a["age_ms"], 10, "twin.a is alpha's record: {a}");
    assert_eq!(b["age_ms"], 0, "twin.b is beta's record: {b}");

    // The assembled object reports the largest age among its records.
    let whole = get(&mut s, "twin").await;
    assert_eq!(whole["data"], serde_json::json!({"a": 1, "b": 2}));
    assert_eq!(
        whole["age_ms"], 10,
        "the object's age is the oldest: {whole}"
    );

    handle.abort();
}

// ── (g) a watch frame carries the template half too ─────────────────────────

#[tokio::test]
async fn a_watch_frame_carries_the_template() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut s = UnixStream::connect(&sock).await.unwrap();
    seed_scanline_template(&mut s).await;

    let mut w = UnixStream::connect(&sock).await.unwrap();
    w.write_all(b"{\"op\":\"watch\",\"key\":\"scanline.widgets.foo\"}\n")
        .await
        .unwrap();
    let mut reader = BufReader::new(&mut w);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let frame: serde_json::Value = serde_json::from_str(line.trim()).unwrap();

    assert_eq!(
        without_age(frame),
        serde_json::json!({"ok": true, "template": "{{ env.SCANLINE_FOO }}"})
    );

    handle.abort();
}

/// A watch subscribes the template chain as well as the data one, and dedups on
/// both halves — so editing the template alone delivers a frame, and so does a
/// value arriving under an unchanged template.
#[tokio::test]
async fn a_watch_wakes_on_a_template_change_and_on_a_value() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut s = UnixStream::connect(&sock).await.unwrap();
    seed_scanline_template(&mut s).await;

    let mut w = UnixStream::connect(&sock).await.unwrap();
    w.write_all(b"{\"op\":\"watch\",\"key\":\"scanline.widgets.foo\"}\n")
        .await
        .unwrap();
    let mut reader = BufReader::new(&mut w);
    let next_frame = async |reader: &mut BufReader<&mut UnixStream>| {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let frame: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        without_age(frame)
    };

    assert_eq!(
        next_frame(&mut reader).await,
        serde_json::json!({"ok": true, "template": "{{ env.SCANLINE_FOO }}"}),
        "the initial frame"
    );

    put(
        &mut s,
        "scanline:template.widgets.foo",
        serde_json::json!("{{ 2 }}"),
    )
    .await;
    assert_eq!(
        next_frame(&mut reader).await,
        serde_json::json!({"ok": true, "template": "{{ 2 }}"}),
        "the template changed on its own"
    );

    put(&mut s, "scanline.widgets", serde_json::json!({"foo": 1})).await;
    assert_eq!(
        next_frame(&mut reader).await,
        serde_json::json!({"ok": true, "data": 1, "stale": false, "template": "{{ 2 }}"}),
        "a value arrived under the same template"
    );

    handle.abort();
}
