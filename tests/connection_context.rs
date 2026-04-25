use beachcomber::cache::Cache;
use beachcomber::protocol::Response;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use beachcomber::server::Server;
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// A path-scoped provider that stores the path it was called with
/// as the "active_path" field value.
struct PathScopedSourceImpl;

impl Source for PathScopedSourceImpl {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(|| SourceMetadata {
            name: "main".into(),
            fields: vec![FieldSchema {
                name: "active_path".into(),
                field_type: FieldType::String,
            }],
            scope: SourceScope::PathScoped,
            invalidation: InvalidationStrategy::Poll { interval_secs: 60 },
            keep_alive: KeepAlive::Polls(2),
            failback: FailbackConfig { reattempts: 3, interval_secs: 30 },
            fsevents_reinstate: false,
        })
    }

    fn execute(&self, path: Option<&str>) -> SourceResult {
        let mut result = SourceResult::new();
        result.insert("active_path", Value::String(path.unwrap_or("<none>").to_string()));
        result
    }
}

struct PathScopedProvider;

impl Provider for PathScopedProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "pathprov".to_string(),
            sources: vec![PathScopedSourceImpl.metadata().clone()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(PathScopedSourceImpl)]
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
            scope: SourceScope::Global,
            invalidation: InvalidationStrategy::Watch {
                patterns: vec![],
                abs_paths: vec![],
            },
            keep_alive: KeepAlive::Never,
            failback: FailbackConfig { reattempts: 3, interval_secs: 30 },
            fsevents_reinstate: false,
        })
    }

    fn execute(&self, _path: Option<&str>) -> SourceResult {
        let mut result = SourceResult::new();
        result.insert("info", Value::String("global-value".to_string()));
        result
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
    registry.register(Box::new(PathScopedProvider)).expect("pathprov");
    registry.register(Box::new(GlobalProvider)).expect("globalprov");
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

/// Test 1: Setting context makes subsequent gets use the context path for path-scoped providers.
#[tokio::test]
async fn context_sets_default_path_for_scoped_providers() {
    let (_tmp, sock, cache, registry, watchers) = setup_with_custom_registry();

    // Pre-populate cache for two different paths
    let mut fields_a = HashMap::new();
    fields_a.insert("active_path".to_string(), Value::String("/project/a".to_string()));
    cache.put_source("pathprov", Some("/project/a"), "main", fields_a, Some(60));

    let mut fields_b = HashMap::new();
    fields_b.insert("active_path".to_string(), Value::String("/project/b".to_string()));
    cache.put_source("pathprov", Some("/project/b"), "main", fields_b, Some(60));

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // Set context to /project/a
    let resp = send_recv_line(
        &mut writer,
        &mut reader,
        r#"{"op":"context","path":"/project/a"}"#,
    )
    .await;
    assert!(resp.ok, "context op should succeed");

    // Get without explicit path — should use /project/a from context
    let resp = send_recv_line(
        &mut writer,
        &mut reader,
        r#"{"op":"get","key":"pathprov.active_path"}"#,
    )
    .await;
    assert!(resp.ok);
    assert_eq!(resp.data.unwrap(), serde_json::json!("/project/a"));

    handle.abort();
}

/// Test 2: An explicit path on a request overrides the context path.
#[tokio::test]
async fn explicit_path_overrides_context() {
    let (_tmp, sock, cache, registry, watchers) = setup_with_custom_registry();

    let mut fields_a = HashMap::new();
    fields_a.insert("active_path".to_string(), Value::String("/project/a".to_string()));
    cache.put_source("pathprov", Some("/project/a"), "main", fields_a, Some(60));

    let mut fields_b = HashMap::new();
    fields_b.insert("active_path".to_string(), Value::String("/project/b".to_string()));
    cache.put_source("pathprov", Some("/project/b"), "main", fields_b, Some(60));

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // Set context to /project/a
    let resp = send_recv_line(
        &mut writer,
        &mut reader,
        r#"{"op":"context","path":"/project/a"}"#,
    )
    .await;
    assert!(resp.ok);

    // Get with explicit path /project/b — should override context
    let resp = send_recv_line(
        &mut writer,
        &mut reader,
        r#"{"op":"get","key":"pathprov.active_path","path":"/project/b"}"#,
    )
    .await;
    assert!(resp.ok);
    assert_eq!(
        resp.data.unwrap(),
        serde_json::json!("/project/b"),
        "Explicit path should override context"
    );

    handle.abort();
}

/// Test 3: Global providers ignore the context path.
#[tokio::test]
async fn global_provider_ignores_context() {
    let (_tmp, sock, cache, registry, watchers) = setup_with_custom_registry();

    // Pre-populate global provider in cache (no path)
    let mut fields = HashMap::new();
    fields.insert("info".to_string(), Value::String("global-value".to_string()));
    cache.put_source("globalprov", None, "main", fields, None);

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // Set context to some path
    let resp = send_recv_line(
        &mut writer,
        &mut reader,
        r#"{"op":"context","path":"/some/dir"}"#,
    )
    .await;
    assert!(resp.ok);

    // Get from global provider — context should be ignored, should find cache entry at None path
    let resp = send_recv_line(
        &mut writer,
        &mut reader,
        r#"{"op":"get","key":"globalprov.info"}"#,
    )
    .await;
    assert!(resp.ok, "Global provider should still be found");
    assert_eq!(
        resp.data.unwrap(),
        serde_json::json!("global-value"),
        "Global provider should ignore context and return global cache entry"
    );

    handle.abort();
}
