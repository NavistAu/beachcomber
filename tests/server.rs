use beachcomber::cache::Cache;
use beachcomber::protocol::Response;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::Value;
use beachcomber::server::Server;
use std::collections::HashMap;
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

#[tokio::test]
async fn server_accepts_connection() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);

    let handle = tokio::spawn(async move { server.run().await });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&sock).await;
    assert!(stream.is_ok(), "Should connect to server socket");

    handle.abort();
}

#[tokio::test]
async fn server_handles_get_global_provider() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    let mut fields = HashMap::new();
    fields.insert("name".to_string(), Value::String("testhost.local".to_string()));
    fields.insert("short".to_string(), Value::String("testhost".to_string()));
    cache.put_source("hostname", None, "main", fields, Some(60));

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op": "get", "key": "hostname"}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok, "Response should be ok");
    let data = response.data.unwrap();
    assert_eq!(data["name"], "testhost.local");
    assert_eq!(data["short"], "testhost");

    handle.abort();
}

#[tokio::test]
async fn server_handles_get_single_field() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    let mut fields = HashMap::new();
    fields.insert("name".to_string(), Value::String("testhost.local".to_string()));
    fields.insert("short".to_string(), Value::String("testhost".to_string()));
    cache.put_source("hostname", None, "main", fields, Some(60));

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op": "get", "key": "hostname.short"}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok);
    assert_eq!(response.data.unwrap(), serde_json::json!("testhost"));

    handle.abort();
}

#[tokio::test]
async fn server_handles_get_text_format() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    let mut fields = HashMap::new();
    fields.insert("name".to_string(), Value::String("testhost.local".to_string()));
    cache.put_source("hostname", None, "main", fields, Some(60));

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op": "get", "key": "hostname.name", "format": "text"}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    // Text format for single field: raw value followed by newline
    assert_eq!(line.trim(), "testhost.local");

    handle.abort();
}

#[tokio::test]
async fn server_handles_cache_miss_with_sync_execution() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    // No cache populated — the server should execute the provider inline
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    // hostname is a global provider that always returns data
    let request = r#"{"op": "get", "key": "hostname"}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok, "Response should be ok");
    assert!(
        response.data.is_some(),
        "Sync cache miss should return data from inline execution"
    );
    let data = response.data.unwrap();
    assert!(
        data.get("name").is_some(),
        "hostname provider should return a name field"
    );

    handle.abort();
}

#[tokio::test]
async fn server_handles_cache_miss_provider_returns_none() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    // git provider with no path — execute() returns None
    let request = r#"{"op": "get", "key": "git"}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(
        response.ok,
        "Response should be ok even when provider returns None"
    );
    assert!(
        response.data.is_none(),
        "Provider returning None should still produce a miss"
    );

    handle.abort();
}

#[tokio::test]
async fn server_handles_cache_miss_virtual_provider() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    // Register a virtual provider name but don't populate cache
    registry.register_virtual("myvirtual");

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op": "get", "key": "myvirtual"}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok);
    assert!(
        response.data.is_none(),
        "Virtual provider with no cache data should return miss (no execute to call)"
    );

    handle.abort();
}

#[tokio::test]
async fn server_handles_unknown_provider() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op": "get", "key": "nonexistent"}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(!response.ok, "Unknown provider should return error");
    assert!(response.error.unwrap().contains("unknown provider"));

    handle.abort();
}

#[tokio::test]
async fn server_handles_refresh() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op": "refresh", "key": "hostname"}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok, "Refresh should return ok");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    handle.abort();
}

#[tokio::test]
async fn server_handles_get_sh_format() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    let mut fields = HashMap::new();
    fields.insert("name".to_string(), Value::String("testhost.local".to_string()));
    fields.insert("short".to_string(), Value::String("testhost".to_string()));
    cache.put_source("hostname", None, "main", fields, Some(60));

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op": "get", "key": "hostname", "format": "sh"}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut lines = String::new();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.unwrap();
        if n == 0 || line == "\n" {
            break;
        }
        lines.push_str(&line);
    }

    // Sh format for objects: sorted key=value pairs
    assert!(lines.contains("name=testhost.local"));
    assert!(lines.contains("short=testhost"));

    handle.abort();
}

#[tokio::test]
async fn server_text_format_object_emits_key_value_lines() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    let mut fields = HashMap::new();
    fields.insert("name".to_string(), Value::String("testhost.local".to_string()));
    fields.insert("short".to_string(), Value::String("testhost".to_string()));
    cache.put_source("hostname", None, "main", fields, Some(60));

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op": "get", "key": "hostname", "format": "text"}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut lines = String::new();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.unwrap();
        if n == 0 || line == "\n" {
            break;
        }
        lines.push_str(&line);
    }

    // Text format for objects: `subkey=value` sorted, one per line.
    // Matches the sh format and the 2026-04-21 code-review-fixes design (C9).
    assert!(
        lines.contains("name=testhost.local"),
        "expected 'name=testhost.local' in output, got: {lines:?}"
    );
    assert!(
        lines.contains("short=testhost"),
        "expected 'short=testhost' in output, got: {lines:?}"
    );

    handle.abort();
}

#[tokio::test]
async fn daemon_introspect_includes_uptime_and_request_counters() {
    // uptime_secs, active_watchers, requests_total moved from Request::Status to
    // Request::Introspect{daemon} in T27.
    let (_tmp, sock, cache, registry, watchers) = setup();

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();

    // Send two get requests first to bump the counter.
    stream
        .write_all(b"{\"op\": \"get\", \"key\": \"hostname\"}\n")
        .await
        .unwrap();
    stream
        .write_all(b"{\"op\": \"get\", \"key\": \"hostname\"}\n")
        .await
        .unwrap();

    // Read and discard the two get responses.
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    line.clear();
    reader.read_line(&mut line).await.unwrap();
    line.clear();

    // Send introspect daemon request — this is where counters now live.
    write_half
        .write_all(b"{\"op\": \"introspect\", \"subject\": \"daemon\"}\n")
        .await
        .unwrap();

    reader.read_line(&mut line).await.unwrap();
    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok, "Introspect daemon response should be ok");
    let data = response.data.unwrap();

    let uptime_secs = data["uptime_secs"].as_u64();
    assert!(
        uptime_secs.is_some(),
        "introspect daemon response should include uptime_secs, got: {data}"
    );

    let active_watchers = data["active_watchers"].as_u64();
    assert!(
        active_watchers.is_some(),
        "introspect daemon response should include active_watchers, got: {data}"
    );

    let requests_total = data["requests_total"].as_u64();
    assert!(
        requests_total.is_some(),
        "introspect daemon response should include requests_total, got: {data}"
    );
    assert!(
        requests_total.unwrap() >= 3,
        "requests_total should be >= 3 (2 gets + 1 introspect), got: {}",
        requests_total.unwrap()
    );

    handle.abort();
}

/// Nested key lookup: `comb g provider.field.subkey` walks into an
/// Object-valued field and returns the subkey's scalar value directly.
/// Uses a virtual provider so there's no path canonicalisation walk to
/// contend with.
#[tokio::test]
async fn nested_key_walks_into_object_field() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    let mut tools_map = std::collections::HashMap::new();
    tools_map.insert("rust".to_string(), Value::String("1.94.0".to_string()));
    tools_map.insert(
        "cargo-nextest".to_string(),
        Value::String("0.9.133".to_string()),
    );
    let mut fields = HashMap::new();
    fields.insert("project".to_string(), Value::Object(tools_map));
    cache.put_source("myproj", None, "virtual", fields, None);
    registry.register_virtual("myproj");

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op":"get","key":"myproj.project.rust","format":"text"}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut buf = String::new();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.unwrap();
        if n == 0 || line == "\n" {
            break;
        }
        buf.push_str(&line);
    }

    assert_eq!(buf.trim(), "1.94.0");

    handle.abort();
}

/// Nested key lookup fails loudly with a clear error when the subkey is absent.
#[tokio::test]
async fn nested_key_missing_subkey_errors() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    let mut tools_map = std::collections::HashMap::new();
    tools_map.insert("rust".to_string(), Value::String("1.94.0".to_string()));
    let mut fields = HashMap::new();
    fields.insert("project".to_string(), Value::Object(tools_map));
    cache.put_source("myproj", None, "virtual", fields, None);
    registry.register_virtual("myproj");

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op":"get","key":"myproj.project.nonesuch","format":"json"}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let resp: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(resp["ok"], false);
    assert!(
        resp["error"]
            .as_str()
            .unwrap()
            .contains("unknown field: myproj.project.nonesuch"),
        "got: {line}"
    );

    handle.abort();
}

/// Format::Text on an Object-valued field emits `subkey=value` lines sorted
/// alphabetically by subkey. Applies to all object-valued provider fields
/// (e.g. mise.project, asdf.tools). Matches the spec at
/// docs/superpowers/specs/2026-04-21-code-review-fixes-design.md C9.
#[tokio::test]
async fn text_format_object_field_emits_subkey_equals_value_lines() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    // Store a provider result with a single Object-valued field ("tools").
    // The Object maps tool names to version strings, mirroring mise.project / asdf.tools.
    let mut tools_map = std::collections::HashMap::new();
    tools_map.insert("node".to_string(), Value::String("20.11.0".to_string()));
    tools_map.insert("python".to_string(), Value::String("3.12.1".to_string()));
    let mut fields = HashMap::new();
    fields.insert("tools".to_string(), Value::Object(tools_map));
    cache.put_source("devenv", None, "virtual", fields, None);
    // Register as virtual so the server's provider-existence guard accepts the name.
    registry.register_virtual("devenv");

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    // Request the Object-valued field directly with format=text.
    let request = r#"{"op": "get", "key": "devenv.tools", "format": "text"}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut lines_buf = String::new();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.unwrap();
        if n == 0 || line == "\n" {
            break;
        }
        lines_buf.push_str(&line);
    }

    let trimmed = lines_buf.trim();
    let output_lines: Vec<&str> = trimmed.split('\n').collect();
    assert_eq!(
        output_lines.len(),
        2,
        "Object with two entries should emit exactly two lines, got: {trimmed:?}"
    );
    // node < python alphabetically, so node's line comes first.
    assert_eq!(output_lines[0], "node=20.11.0");
    assert_eq!(output_lines[1], "python=3.12.1");

    handle.abort();
}
