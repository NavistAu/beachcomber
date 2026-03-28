use beachcomber::cache::Cache;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{
    Provider, ProviderMetadata, ProviderResult, Value,
    FieldSchema, FieldType, InvalidationStrategy,
};
use beachcomber::server::Server;
use beachcomber::protocol::Response;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::io::{AsyncWriteExt, AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;

/// A path-scoped provider that stores the path it was called with
/// as the "active_path" field value.
struct PathScopedProvider;

impl Provider for PathScopedProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "pathprov".to_string(),
            fields: vec![
                FieldSchema { name: "active_path".to_string(), field_type: FieldType::String },
            ],
            invalidation: InvalidationStrategy::Poll {
                interval_secs: 60,
                floor_secs: 1,
            },
            global: false,
        }
    }

    fn execute(&self, path: Option<&str>) -> Option<ProviderResult> {
        let mut result = ProviderResult::new();
        let val = path.unwrap_or("<none>").to_string();
        result.insert("active_path", Value::String(val));
        Some(result)
    }
}

/// A global provider — context should NOT affect it.
struct GlobalProvider;

impl Provider for GlobalProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "globalprov".to_string(),
            fields: vec![
                FieldSchema { name: "info".to_string(), field_type: FieldType::String },
            ],
            invalidation: InvalidationStrategy::Once,
            global: true,
        }
    }

    fn execute(&self, _path: Option<&str>) -> Option<ProviderResult> {
        let mut result = ProviderResult::new();
        result.insert("info", Value::String("global-value".to_string()));
        Some(result)
    }
}

fn setup_with_custom_registry() -> (TempDir, std::path::PathBuf, Arc<Cache>, Arc<ProviderRegistry>) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("test.sock");
    let cache = Arc::new(Cache::new());
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(PathScopedProvider));
    registry.register(Box::new(GlobalProvider));
    let registry = Arc::new(registry);
    (tmp, sock, cache, registry)
}

async fn send_recv_line(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    request: &str,
) -> Response {
    writer.write_all(format!("{}\n", request).as_bytes()).await.unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    serde_json::from_str(&line).unwrap()
}

/// Test 1: Setting context makes subsequent gets use the context path for path-scoped providers.
#[tokio::test]
async fn context_sets_default_path_for_scoped_providers() {
    let (_tmp, sock, cache, registry) = setup_with_custom_registry();

    // Pre-populate cache for two different paths
    let mut result_a = ProviderResult::new();
    result_a.insert("active_path", Value::String("/project/a".to_string()));
    cache.put("pathprov", Some("/project/a"), result_a);

    let mut result_b = ProviderResult::new();
    result_b.insert("active_path", Value::String("/project/b".to_string()));
    cache.put("pathprov", Some("/project/b"), result_b);

    let server = Server::new(sock.clone(), cache, registry, None);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // Set context to /project/a
    let resp = send_recv_line(&mut writer, &mut reader, r#"{"op":"context","path":"/project/a"}"#).await;
    assert!(resp.ok, "context op should succeed");

    // Get without explicit path — should use /project/a from context
    let resp = send_recv_line(&mut writer, &mut reader, r#"{"op":"get","key":"pathprov.active_path"}"#).await;
    assert!(resp.ok);
    assert_eq!(resp.data.unwrap(), serde_json::json!("/project/a"));

    handle.abort();
}

/// Test 2: An explicit path on a request overrides the context path.
#[tokio::test]
async fn explicit_path_overrides_context() {
    let (_tmp, sock, cache, registry) = setup_with_custom_registry();

    let mut result_a = ProviderResult::new();
    result_a.insert("active_path", Value::String("/project/a".to_string()));
    cache.put("pathprov", Some("/project/a"), result_a);

    let mut result_b = ProviderResult::new();
    result_b.insert("active_path", Value::String("/project/b".to_string()));
    cache.put("pathprov", Some("/project/b"), result_b);

    let server = Server::new(sock.clone(), cache, registry, None);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // Set context to /project/a
    let resp = send_recv_line(&mut writer, &mut reader, r#"{"op":"context","path":"/project/a"}"#).await;
    assert!(resp.ok);

    // Get with explicit path /project/b — should override context
    let resp = send_recv_line(
        &mut writer, &mut reader,
        r#"{"op":"get","key":"pathprov.active_path","path":"/project/b"}"#,
    ).await;
    assert!(resp.ok);
    assert_eq!(resp.data.unwrap(), serde_json::json!("/project/b"),
        "Explicit path should override context");

    handle.abort();
}

/// Test 3: Global providers ignore the context path.
#[tokio::test]
async fn global_provider_ignores_context() {
    let (_tmp, sock, cache, registry) = setup_with_custom_registry();

    // Pre-populate global provider in cache (no path)
    let mut result = ProviderResult::new();
    result.insert("info", Value::String("global-value".to_string()));
    cache.put("globalprov", None, result);

    let server = Server::new(sock.clone(), cache, registry, None);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // Set context to some path
    let resp = send_recv_line(&mut writer, &mut reader, r#"{"op":"context","path":"/some/dir"}"#).await;
    assert!(resp.ok);

    // Get from global provider — context should be ignored, should find cache entry at None path
    let resp = send_recv_line(&mut writer, &mut reader, r#"{"op":"get","key":"globalprov.info"}"#).await;
    assert!(resp.ok, "Global provider should still be found");
    assert_eq!(resp.data.unwrap(), serde_json::json!("global-value"),
        "Global provider should ignore context and return global cache entry");

    handle.abort();
}
