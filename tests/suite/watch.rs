use beachcomber::cache::Cache;
use beachcomber::cache::Schedule;
use beachcomber::protocol::Response;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    Source, SourceMetadata, SourceResult, Value,
};
use beachcomber::server::Server;
use beachcomber::watcher_registry::WatcherRegistry;
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::common::poll::poll_until_eq_async;

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
    let mut registry = crate::common::registry::with_shipped_defaults();
    registry.register(Box::new(Watched)).unwrap();
    (tmp, sock, cache, Arc::new(registry), watchers)
}

/// A provider with one Source and no root: its one instance is at `~`, so the
/// records these tests put there are what a read at any directory sees. (git
/// is nearest, and a directory outside a repository has no git instance.)
struct Watched;

fn watched_meta() -> SourceMetadata {
    SourceMetadata {
        name: "main".into(),
        fields: vec![
            FieldSchema {
                name: "branch".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "dirty".into(),
                field_type: FieldType::Bool,
            },
        ],
        invalidation: InvalidationStrategy::Poll {
            interval_secs: 60,
            count: 1,
        },
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 1,
        },
        fsevents_reinstate: false,
        decay: None,
        budget_execute: beachcomber::scheduler::lifecycle::DEFAULT_BUDGET_EXECUTE,
        timeout: beachcomber::scheduler::lifecycle::DEFAULT_TIMEOUT,
        nearest: false,
    }
}

impl Provider for Watched {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "watched".into(),
            sources: vec![watched_meta()],
        }
    }
    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(Watched)]
    }
}

impl Source for Watched {
    fn metadata(&self) -> &SourceMetadata {
        static M: std::sync::OnceLock<SourceMetadata> = std::sync::OnceLock::new();
        M.get_or_init(watched_meta)
    }
    fn execute(&self, _dir: Option<&str>) -> Result<SourceResult, String> {
        Ok(SourceResult::new())
    }
}

fn git_fields(branch: &str, dirty: bool) -> HashMap<String, Value> {
    let mut fields = HashMap::new();
    fields.insert("branch".to_string(), Value::String(branch.to_string()));
    fields.insert("dirty".to_string(), Value::Bool(dirty));
    fields
}

#[tokio::test]
async fn watch_receives_initial_value() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    cache.put_source(
        "watched",
        None,
        "main",
        git_fields("main", false),
        Schedule::poll(60, 1),
    );

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    writer
        .write_all(b"{\"op\":\"watch\",\"key\":\"watched.branch\"}\n")
        .await
        .unwrap();

    let mut line = String::new();
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        reader.read_line(&mut line),
    )
    .await
    .expect("timed out waiting for initial watch value")
    .unwrap();
    assert!(result > 0, "Expected a response line");

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok, "Response should be ok: {:?}", response.error);
    assert_eq!(
        response.data.unwrap(),
        serde_json::json!("main"),
        "Initial value should be 'main'"
    );

    handle.abort();
}

#[tokio::test]
async fn watch_receives_updates() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    cache.put_source(
        "watched",
        None,
        "main",
        git_fields("main", false),
        Schedule::poll(60, 1),
    );
    let cache_writer = cache.clone();

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    writer
        .write_all(b"{\"op\":\"watch\",\"key\":\"watched.branch\"}\n")
        .await
        .unwrap();

    // Read initial value
    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_millis(500),
        reader.read_line(&mut line),
    )
    .await
    .expect("timed out waiting for initial value")
    .unwrap();
    let response: Response = serde_json::from_str(&line).unwrap();
    assert_eq!(response.data.unwrap(), serde_json::json!("main"));

    // Update cache with new branch
    cache_writer.put_source(
        "watched",
        None,
        "main",
        git_fields("feature/new-branch", false),
        Schedule::poll(60, 1),
    );

    // Read the update
    line.clear();
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        reader.read_line(&mut line),
    )
    .await
    .expect("timed out waiting for update")
    .unwrap();
    assert!(result > 0, "Expected an update line");

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok);
    assert_eq!(
        response.data.unwrap(),
        serde_json::json!("feature/new-branch"),
        "Should receive updated branch name"
    );

    handle.abort();
}

#[tokio::test]
async fn watch_filters_unchanged_field() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    cache.put_source(
        "watched",
        None,
        "main",
        git_fields("main", false),
        Schedule::poll(60, 1),
    );
    let cache_writer = cache.clone();

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    writer
        .write_all(b"{\"op\":\"watch\",\"key\":\"watched.branch\"}\n")
        .await
        .unwrap();

    // Read initial value
    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_millis(500),
        reader.read_line(&mut line),
    )
    .await
    .expect("timed out waiting for initial value")
    .unwrap();
    let response: Response = serde_json::from_str(&line).unwrap();
    assert_eq!(response.data.unwrap(), serde_json::json!("main"));

    // Update cache — change only dirty, branch stays "main"
    cache_writer.put_source(
        "watched",
        None,
        "main",
        git_fields("main", true),
        Schedule::poll(60, 1),
    );

    // Should NOT receive any update — branch didn't change
    line.clear();
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        reader.read_line(&mut line),
    )
    .await;
    assert!(
        result.is_err(),
        "Should timeout — no update expected when watched field is unchanged"
    );

    handle.abort();
}

#[tokio::test]
async fn watch_provider_level_gets_all_changes() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    cache.put_source(
        "watched",
        None,
        "main",
        git_fields("main", false),
        Schedule::poll(60, 1),
    );
    let cache_writer = cache.clone();

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // Watch "watched" — no field, gets whole provider
    writer
        .write_all(b"{\"op\":\"watch\",\"key\":\"watched\"}\n")
        .await
        .unwrap();

    // Read initial value
    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_millis(500),
        reader.read_line(&mut line),
    )
    .await
    .expect("timed out waiting for initial value")
    .unwrap();
    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok);
    let data = response.data.unwrap();
    assert_eq!(data["branch"], "main");

    // Update cache — change only dirty, branch stays "main"
    // Provider-level watch should still fire since the overall object changed
    cache_writer.put_source(
        "watched",
        None,
        "main",
        git_fields("main", true),
        Schedule::poll(60, 1),
    );

    // Should receive an update
    line.clear();
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        reader.read_line(&mut line),
    )
    .await
    .expect("timed out waiting for update")
    .unwrap();
    assert!(result > 0, "Expected an update line");

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok);
    let data = response.data.unwrap();
    assert_eq!(data["dirty"], true, "dirty should now be true");

    handle.abort();
}

#[tokio::test]
async fn watch_miss_then_update() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    // Do NOT pre-populate cache
    let cache_writer = cache.clone();

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    writer
        .write_all(b"{\"op\":\"watch\",\"key\":\"watched.branch\"}\n")
        .await
        .unwrap();

    // Read initial value — should be a miss (data is None)
    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_millis(500),
        reader.read_line(&mut line),
    )
    .await
    .expect("timed out waiting for initial miss response")
    .unwrap();
    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok, "Miss should still be ok");
    assert!(
        response.data.is_none(),
        "Initial miss should have no data: {:?}",
        response.data
    );

    // Now populate the cache
    cache_writer.put_source(
        "watched",
        None,
        "main",
        git_fields("main", false),
        Schedule::poll(60, 1),
    );

    // Should receive the populated value
    line.clear();
    let bytes_read = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        reader.read_line(&mut line),
    )
    .await
    .expect("timed out waiting for update after cache population")
    .unwrap();
    assert!(bytes_read > 0, "Expected an update line");

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok);
    assert_eq!(
        response.data.unwrap(),
        serde_json::json!("main"),
        "Should receive the branch value after cache is populated"
    );

    handle.abort();
}

/// Read one watch frame, failing on a deadline rather than hanging.
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

/// Open a watch on `key` and consume its initial frame.
async fn open_watch(
    sock: &std::path::Path,
    key: &str,
) -> (
    BufReader<tokio::net::unix::OwnedReadHalf>,
    tokio::net::unix::OwnedWriteHalf,
    Response,
) {
    let stream = UnixStream::connect(sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let req = format!("{{\"op\":\"watch\",\"key\":\"{key}\"}}\n");
    writer.write_all(req.as_bytes()).await.unwrap();
    let first = next_frame(&mut reader).await;
    (reader, writer, first)
}

#[tokio::test]
async fn a_watch_on_a_namespace_nothing_answers_wakes_on_its_first_put() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let writer = cache.clone();
    let handle = Server::new(sock.clone(), cache, registry, None, watchers)
        .spawn()
        .unwrap();

    let (mut reader, _w, first) = open_watch(&sock, "nowhere.x").await;
    assert!(first.ok, "a watch on an unknown namespace is accepted");
    assert!(first.data.is_none(), "and its first frame is a miss");

    writer
        .write_static("nowhere", None, "put", "x", Value::String("1".into()), None)
        .unwrap();
    let frame = next_frame(&mut reader).await;
    assert_eq!(frame.data.unwrap(), serde_json::json!("1"));
    handle.abort();
}

#[tokio::test]
async fn a_template_put_wakes_a_watch_on_the_field() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let writer = cache.clone();
    let handle = Server::new(sock.clone(), cache, registry, None, watchers)
        .spawn()
        .unwrap();

    let (mut reader, _w, first) = open_watch(&sock, "nowhere.x").await;
    assert!(first.ok && first.data.is_none());

    writer
        .write_static(
            "nowhere:template",
            None,
            "put",
            "x",
            Value::String("{{ 1 }}".into()),
            None,
        )
        .unwrap();
    let frame = next_frame(&mut reader).await;
    assert!(frame.ok, "{:?}", frame.error);
    assert!(
        frame.template.is_some(),
        "the template put produced a frame carrying the template: {frame:?}"
    );
    handle.abort();
}

#[tokio::test]
async fn a_watch_on_the_put_facet_wakes_on_a_data_write() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let writer = cache.clone();
    let handle = Server::new(sock.clone(), cache, registry, None, watchers)
        .spawn()
        .unwrap();

    let (mut reader, _w, first) = open_watch(&sock, "nowhere:put.x").await;
    assert!(first.ok && first.data.is_none(), "{first:?}");

    writer
        .write_static("nowhere", None, "put", "x", Value::String("1".into()), None)
        .unwrap();
    let frame = next_frame(&mut reader).await;
    assert_eq!(frame.data.unwrap(), serde_json::json!("1"));
    handle.abort();
}

/// `active_watchers` counts open watch streams, one per client — not links of
/// the watcher registry. A watch subscribes to every entry on its path's chain,
/// so counting registry entries reported a single watch at depth six as six.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn introspect_counts_one_watch_once() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    cache.put_source(
        "git",
        None,
        "refs",
        git_fields("main", false),
        Schedule::poll(60, 1),
    );
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    // A watch at a nested path, so its chain has several links.
    let deep = std::env::current_dir().unwrap().join("a/b/c/d/e/f");
    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let req = serde_json::json!({
        "op": "watch", "key": "git.branch", "path": deep.to_string_lossy(),
    });
    writer
        .write_all(format!("{req}\n").as_bytes())
        .await
        .unwrap();
    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_millis(2000),
        reader.read_line(&mut line),
    )
    .await
    .expect("the watch must open")
    .unwrap();

    let mut probe = UnixStream::connect(&sock).await.unwrap();
    probe
        .write_all(b"{\"op\":\"introspect\",\"subject\":\"daemon\"}\n")
        .await
        .unwrap();
    let mut probe_reader = BufReader::new(&mut probe);
    let mut out = String::new();
    probe_reader.read_line(&mut out).await.unwrap();
    let resp: Response = serde_json::from_str(&out).unwrap();
    let data = resp.data.expect("introspect returns data");

    assert_eq!(
        data["active_watchers"],
        serde_json::json!(1),
        "one open watch is one watcher, whatever its chain depth"
    );

    handle.abort();
}

/// Query `introspect daemon` on a fresh probe connection and return
/// `active_watchers` — the count of open `watch` streams (see `WatchGuard`).
async fn active_watchers(sock: &std::path::Path) -> u64 {
    let mut probe = UnixStream::connect(sock).await.unwrap();
    probe
        .write_all(b"{\"op\":\"introspect\",\"subject\":\"daemon\"}\n")
        .await
        .unwrap();
    let mut probe_reader = BufReader::new(&mut probe);
    let mut out = String::new();
    probe_reader.read_line(&mut out).await.unwrap();
    let resp: Response = serde_json::from_str(&out).unwrap();
    resp.data.expect("introspect returns data")["active_watchers"]
        .as_u64()
        .unwrap()
}

/// A dead client's watch must not linger until the next write to the watched
/// key: the daemon notices the read half completing (EOF here) and drops the
/// subscription at once — proven by `active_watchers` returning to zero with
/// no write to `git.branch` ever happening.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watch_ends_when_client_disconnects_without_a_write() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    cache.put_source(
        "git",
        None,
        "refs",
        git_fields("main", false),
        Schedule::poll(60, 1),
    );

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    writer
        .write_all(b"{\"op\":\"watch\",\"key\":\"git.branch\"}\n")
        .await
        .unwrap();

    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_millis(500),
        reader.read_line(&mut line),
    )
    .await
    .expect("timed out waiting for initial value")
    .unwrap();

    assert_eq!(
        active_watchers(&sock).await,
        1,
        "watch should be open before the client goes away"
    );

    // The client goes away. It never writes to git.branch again — if the
    // daemon only noticed disconnects on the next write, this watch would
    // linger forever.
    drop(reader);
    drop(writer);

    poll_until_eq_async(
        "active_watchers after the client disconnects",
        std::time::Duration::from_secs(2),
        std::time::Duration::from_millis(10),
        async || active_watchers(&sock).await,
        0,
    )
    .await;

    handle.abort();
}

/// Inverse of the above: a client that stays connected keeps its watch open
/// across the same wait — disconnect detection must not be trigger-happy
/// against a live, merely-idle client.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watch_stays_open_while_client_is_connected() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    cache.put_source(
        "git",
        None,
        "refs",
        git_fields("main", false),
        Schedule::poll(60, 1),
    );

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    writer
        .write_all(b"{\"op\":\"watch\",\"key\":\"git.branch\"}\n")
        .await
        .unwrap();

    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_millis(500),
        reader.read_line(&mut line),
    )
    .await
    .expect("timed out waiting for initial value")
    .unwrap();

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(300);
    loop {
        assert_eq!(
            active_watchers(&sock).await,
            1,
            "a connected client's watch must not be dropped"
        );
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        // sleep-justified: proving the watch does NOT close has no condition to poll_until — sample active_watchers across the wait window instead.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    drop(reader);
    drop(writer);
    handle.abort();
}
