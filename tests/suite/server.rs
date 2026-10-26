use beachcomber::cache::Cache;
use beachcomber::cache::Schedule;
use beachcomber::protocol::Response;
use beachcomber::provider::Value;
use beachcomber::provider::registry::ProviderRegistry;
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
    let registry = Arc::new(crate::common::registry::with_shipped_defaults());
    (tmp, sock, cache, registry, watchers)
}

#[tokio::test]
async fn server_accepts_connection() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);

    let handle = server.spawn().unwrap();

    let stream = UnixStream::connect(&sock).await;
    assert!(stream.is_ok(), "Should connect to server socket");

    handle.abort();
}

#[tokio::test]
async fn server_handles_get_global_provider() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    let mut fields = HashMap::new();
    fields.insert(
        "name".to_string(),
        Value::String("testhost.local".to_string()),
    );
    fields.insert("short".to_string(), Value::String("testhost".to_string()));
    cache.put_source("hostname", None, "host", fields, Schedule::poll(60, 1));

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

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
    fields.insert(
        "name".to_string(),
        Value::String("testhost.local".to_string()),
    );
    fields.insert("short".to_string(), Value::String("testhost".to_string()));
    cache.put_source("hostname", None, "host", fields, Schedule::poll(60, 1));

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

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
async fn server_handles_cache_miss_with_sync_execution() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    // No cache populated — the server should execute the provider inline
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

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
    // A pathless read addresses the global entry, whose directory is `$HOME`,
    // so a path-scoped source runs there. Point `$HOME` at a plain temp
    // directory: on a host whose real home sits inside a repository this test
    // would otherwise be asserting a miss against a live git checkout.
    // SAFETY: nextest gives every test its own process.
    let home = tempfile::TempDir::new().unwrap();
    unsafe { std::env::set_var("HOME", home.path()) };

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    // git provider with nothing to read at $HOME — every source returns empty
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
async fn server_handles_cache_miss_static_namespace() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    // Register a static namespace name but don't populate cache
    registry.register_static("mystatic");

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op": "get", "key": "mystatic"}"#;
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
        "Static namespace with no cache data should return miss (no execute to call)"
    );

    handle.abort();
}

#[tokio::test]
async fn server_handles_unknown_provider() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

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
    assert!(response.ok, "an unknown provider is a miss, not an error");
    assert!(response.data.is_none());

    handle.abort();
}

#[tokio::test]
async fn server_handles_refresh() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

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

    handle.abort();
}

#[tokio::test]
async fn daemon_introspect_includes_uptime_and_request_counters() {
    // uptime_secs, active_watchers, requests_total moved from Request::Status to
    // Request::Introspect{daemon} in T27.
    let (_tmp, sock, cache, registry, watchers) = setup();

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

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
/// Uses a static namespace so there's no path canonicalisation walk to
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
    cache.put_source(
        "myproj",
        None,
        beachcomber::cache::PUT_SOURCE,
        fields,
        Schedule::default(),
    );
    registry.register_static("myproj");

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op":"get","key":"myproj.project.rust"}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok);
    assert_eq!(response.data.unwrap(), serde_json::json!("1.94.0"));

    handle.abort();
}

/// An address into an existing object that has no such key is a miss, like a
/// missing top-level field — not an error. The same rule the client applies
/// when it walks an address past a template.
#[tokio::test]
async fn nested_key_missing_subkey_is_a_miss() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    let mut tools_map = std::collections::HashMap::new();
    tools_map.insert("rust".to_string(), Value::String("1.94.0".to_string()));
    let mut fields = HashMap::new();
    fields.insert("project".to_string(), Value::Object(tools_map));
    cache.put_source(
        "myproj",
        None,
        beachcomber::cache::PUT_SOURCE,
        fields,
        Schedule::default(),
    );
    registry.register_static("myproj");

    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op":"get","key":"myproj.project.nonesuch"}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let resp: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(resp["ok"], true, "a miss is ok: {line}");
    assert!(resp.get("data").is_none(), "a miss carries no data: {line}");
    assert!(
        resp.get("error").is_none(),
        "a miss is not an error: {line}"
    );

    handle.abort();
}

/// Sends one `get` to the server and returns the response.
async fn get_once(sock: &std::path::Path, key: &str) -> Response {
    let mut stream = UnixStream::connect(sock).await.unwrap();
    let request = format!(r#"{{"op": "get", "key": "{key}"}}"#);
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).await.unwrap();
    serde_json::from_str(&line).unwrap()
}

/// A read the put layer answers neither runs nor demands the Source it hides.
#[tokio::test]
async fn a_put_answered_get_runs_no_source_and_sends_no_demand() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    cache
        .write_static(
            "hostname",
            None,
            "put",
            "name",
            Value::String("x".to_string()),
            None,
        )
        .unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let server = Server::new(
        sock.clone(),
        cache.clone(),
        registry,
        Some(beachcomber::scheduler::SchedulerHandle::new(tx)),
        watchers,
    );
    let handle = server.spawn().unwrap();

    let response = get_once(&sock, "hostname.name").await;
    assert_eq!(response.data.unwrap(), "x");
    assert!(cache.get_source("hostname", None, "host").is_none());
    assert!(
        rx.try_recv().is_err(),
        "a put-answered read demands nothing"
    );

    handle.abort();
}

/// A sibling the put does not cover still reaches the Source layer.
#[tokio::test]
async fn a_get_of_a_field_the_put_does_not_cover_runs_and_demands_the_source() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    cache
        .write_static(
            "hostname",
            None,
            "put",
            "name",
            Value::String("x".to_string()),
            None,
        )
        .unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let server = Server::new(
        sock.clone(),
        cache.clone(),
        registry,
        Some(beachcomber::scheduler::SchedulerHandle::new(tx)),
        watchers,
    );
    let handle = server.spawn().unwrap();

    let response = get_once(&sock, "hostname.short").await;
    assert!(response.ok, "{:?}", response.error);
    assert!(cache.get_source("hostname", None, "host").is_some());
    assert!(rx.try_recv().is_ok(), "a Source-layer read demands");

    handle.abort();
}

/// A request's path is absolute: the client makes it so, against the
/// caller's directory and `$HOME`, which the daemon cannot see. A relative,
/// empty or `~` path is refused on every op that takes one, never joined to
/// the daemon's own working directory.
#[tokio::test]
async fn a_path_that_is_not_absolute_is_refused_on_every_op() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    for path in ["rel/x", "", "~/x"] {
        for request in [
            serde_json::json!({"op": "get", "key": "hostname", "path": path}),
            serde_json::json!({"op": "put", "key": "rp", "data": {"a": 1}, "path": path}),
            serde_json::json!({"op": "refresh", "key": "hostname", "path": path}),
            serde_json::json!({"op": "layers", "key": "rp", "path": path}),
        ] {
            let mut stream = UnixStream::connect(&sock).await.unwrap();
            stream
                .write_all(format!("{request}\n").as_bytes())
                .await
                .unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let response: Response = serde_json::from_str(&line).unwrap();
            assert!(!response.ok, "{request} must be refused: {line}");
            assert_eq!(
                response.error.as_deref(),
                Some(format!("path must be absolute: {path:?}").as_str()),
                "{request}"
            );
        }
    }

    handle.abort();
}

/// A request names only the fields its op takes. A field the op does not
/// know — a retired one, a typo — is refused, never silently ignored.
#[tokio::test]
async fn a_request_field_the_op_does_not_take_is_refused() {
    let (_tmp, sock, cache, registry, watchers) = setup();
    let server = Server::new(sock.clone(), cache, registry, None, watchers);
    let handle = server.spawn().unwrap();

    for request in [
        serde_json::json!({"op": "get", "key": "hostname", "format": "text"}),
        serde_json::json!({"op": "put", "key": "rp", "data": {"a": 1}, "tll": "1m"}),
        serde_json::json!({"op": "refresh", "key": "hostname", "fource": true}),
        serde_json::json!({"op": "status", "verbose": true}),
        serde_json::json!({"op": "hello", "client": "x"}),
    ] {
        let mut stream = UnixStream::connect(&sock).await.unwrap();
        stream
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let response: Response = serde_json::from_str(&line).unwrap();
        assert!(!response.ok, "{request} must be refused: {line}");
        assert!(
            response
                .error
                .as_deref()
                .is_some_and(|e| e.contains("unknown field")),
            "{request}: {line}"
        );
    }

    handle.abort();
}
