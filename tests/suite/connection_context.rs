use beachcomber::cache::Cache;
use beachcomber::cache::Schedule;
use beachcomber::protocol::Response;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    Source, SourceMetadata, SourceResult, Value,
};
use beachcomber::server::Server;
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// A rooted provider that stores the path it was called with
/// as the "active_path" field value.
struct RootedSourceImpl;

impl Source for RootedSourceImpl {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(|| SourceMetadata {
            name: "main".into(),
            fields: vec![FieldSchema {
                name: "active_path".into(),
                field_type: FieldType::String,
            }],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 60,
                count: 2,
            },
            failback: FailbackConfig {
                reattempts: 3,
                interval_secs: 30,
            },
            decay: None,
            budget: beachcomber::scheduler::lifecycle::DEFAULT_BUDGET,
            timeout: beachcomber::scheduler::lifecycle::DEFAULT_TIMEOUT,
            nearest: false,
        })
    }

    fn execute(&self, path: Option<&str>) -> Result<SourceResult, String> {
        let mut result = SourceResult::new();
        result.insert(
            "active_path",
            Value::String(path.unwrap_or("<none>").to_string()),
        );
        Ok(result)
    }

    fn root(&self, dir: &str) -> Result<Option<String>, String> {
        Ok(Some(dir.to_string()))
    }

    fn has_root(&self) -> bool {
        true
    }
}

struct RootedProvider;

impl Provider for RootedProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "pathprov".to_string(),
            sources: vec![RootedSourceImpl.metadata().clone()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(RootedSourceImpl)]
    }
}

/// A global provider — context should NOT affect it.
struct GlobalSourceImpl;

impl Source for GlobalSourceImpl {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(|| SourceMetadata {
            name: "main".into(),
            fields: vec![FieldSchema {
                name: "info".into(),
                field_type: FieldType::String,
            }],
            invalidation: InvalidationStrategy::Watch {
                patterns: vec![],
                abs_paths: vec![],
            },
            failback: FailbackConfig {
                reattempts: 3,
                interval_secs: 30,
            },
            decay: None,
            budget: beachcomber::scheduler::lifecycle::DEFAULT_BUDGET,
            timeout: beachcomber::scheduler::lifecycle::DEFAULT_TIMEOUT,
            nearest: false,
        })
    }

    fn execute(&self, _path: Option<&str>) -> Result<SourceResult, String> {
        let mut result = SourceResult::new();
        result.insert("info", Value::String("global-value".to_string()));
        Ok(result)
    }
}

struct GlobalProvider;

impl Provider for GlobalProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "globalprov".to_string(),
            sources: vec![GlobalSourceImpl.metadata().clone()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(GlobalSourceImpl)]
    }
}

fn setup_with_custom_registry() -> (
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
    let mut registry = ProviderRegistry::new();
    registry
        .register(Box::new(RootedProvider))
        .expect("pathprov");
    registry
        .register(Box::new(GlobalProvider))
        .expect("globalprov");
    let registry = Arc::new(registry);
    (tmp, sock, cache, registry, watchers)
}

async fn send_recv_line(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    request: &str,
) -> Response {
    writer
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    serde_json::from_str(&line).unwrap()
}

/// Test 1: a request's path addresses the rooted instance for that directory.
#[tokio::test]
async fn a_request_path_addresses_its_instance() {
    let (_tmp, sock, cache, registry, watchers) = setup_with_custom_registry();

    // Pre-populate cache for two different paths
    let mut fields_a = HashMap::new();
    fields_a.insert(
        "active_path".to_string(),
        Value::String("/project/a".to_string()),
    );
    cache.put_source(
        "pathprov",
        Some("/project/a"),
        "main",
        fields_a,
        Schedule::poll(60, 1),
    );

    let mut fields_b = HashMap::new();
    fields_b.insert(
        "active_path".to_string(),
        Value::String("/project/b".to_string()),
    );
    cache.put_source(
        "pathprov",
        Some("/project/b"),
        "main",
        fields_b,
        Schedule::poll(60, 1),
    );

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let resp = send_recv_line(
        &mut writer,
        &mut reader,
        r#"{"op":"get","key":"pathprov.active_path","path":"/project/a"}"#,
    )
    .await;
    assert!(resp.ok);
    assert_eq!(resp.data.unwrap(), serde_json::json!("/project/a"));

    handle.abort();
}

/// Test 2: the wire has no `context` op. A connection holds no directory of
/// its own: every request carries its path, and an SDK session that keeps a
/// directory sends it with each request.
#[tokio::test]
async fn the_wire_has_no_context_op() {
    let (_tmp, sock, cache, registry, watchers) = setup_with_custom_registry();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let resp = send_recv_line(
        &mut writer,
        &mut reader,
        r#"{"op":"context","path":"/project/a"}"#,
    )
    .await;
    assert!(!resp.ok, "context is not an op: {resp:?}");

    handle.abort();
}

/// Test 3: a global provider answers a read at any path from its one instance.
#[tokio::test]
async fn global_provider_answers_any_path() {
    let (_tmp, sock, cache, registry, watchers) = setup_with_custom_registry();

    // Pre-populate global provider in cache (no path)
    let mut fields = HashMap::new();
    fields.insert(
        "info".to_string(),
        Value::String("global-value".to_string()),
    );
    cache.put_source("globalprov", None, "main", fields, Schedule::default());

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let resp = send_recv_line(
        &mut writer,
        &mut reader,
        r#"{"op":"get","key":"globalprov.info","path":"/some/dir"}"#,
    )
    .await;
    assert!(resp.ok, "Global provider should still be found");
    assert_eq!(
        resp.data.unwrap(),
        serde_json::json!("global-value"),
        "Global provider should answer from its global cache entry"
    );

    handle.abort();
}

/// Test 4: a rooted instance that has the key answers it; the instance at `~`
/// is only asked when the key is missing.
///
/// Static records resolve up the directory chain to `$HOME`, nearest winning
/// (see tests/suite/store.rs). A Source's records live at its instances: the
/// one at the root that answers for the directory, and the one at `~`, asked
/// only when the nearer one lacks the key. /project/a and /project/b holding
/// different values is the whole point of roots, and a stray global record
/// must never shadow a rooted one. Here the instance at the requested path is
/// cold, so the normal cold-miss inline execute answers with the requested
/// path, and the walk stops there — the seeded global record is not the answer.
///
/// This is the guard against the walk going to the instance at `~` before the
/// nearer one has answered.
#[tokio::test]
async fn a_rooted_instance_that_has_the_key_never_defers_to_the_global_one() {
    let (_tmp, sock, cache, registry, watchers) = setup_with_custom_registry();

    // A record at the instance at `~` — reachable by a pathless read, and by a
    // path-scoped one only when the nearer instance lacks the key.
    let mut fields = HashMap::new();
    fields.insert(
        "active_path".to_string(),
        Value::String("global-sentinel".to_string()),
    );
    cache.put_source("pathprov", None, "main", fields, Schedule::poll(60, 1));

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let resp = send_recv_line(
        &mut writer,
        &mut reader,
        r#"{"op":"get","key":"pathprov.active_path","path":"/project/zz"}"#,
    )
    .await;
    assert!(resp.ok, "get should succeed: {:?}", resp.error);
    assert_eq!(
        resp.data.unwrap(),
        serde_json::json!("/project/zz"),
        "a rooted instance must execute at its root, and the global entry is not asked"
    );

    handle.abort();
}
