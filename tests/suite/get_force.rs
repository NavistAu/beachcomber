use beachcomber::cache::Schedule;
use beachcomber::cache::{Cache, PUT_SOURCE};
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
    let registry = Arc::new(ProviderRegistry::with_defaults());
    (tmp, sock, cache, registry, watchers)
}

/// force=true: evicts the stale cache entry and re-executes the provider,
/// returning age_ms=0 (fresh inline execution) rather than the old age.
#[tokio::test]
async fn get_force_evicts_and_re_executes() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    // Pre-seed the cache with a value for the hostname provider so a normal
    // get would return a hit with age > 0. The map has to be named for the
    // Source that owns it — `host` — because coldness is decided per Source: a
    // map under any other name leaves `host` cold and the read executes it.
    let mut fields = HashMap::new();
    fields.insert("name".to_string(), Value::String("stale-host".to_string()));
    fields.insert("short".to_string(), Value::String("stale".to_string()));
    cache.put_source("hostname", None, "host", fields, Schedule::poll(60, 1));

    // Confirm the cache has the stale entry.
    assert!(
        cache.get_entry("hostname", None).is_some(),
        "cache should have the seeded entry"
    );

    let server = Server::new(sock.clone(), Arc::clone(&cache), registry, None, watchers);
    let handle = server.spawn().unwrap();

    // Send a force=true get for hostname.
    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op":"get","key":"hostname","force":true}"#;
    let asked = std::time::Instant::now();
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let since_asked = asked.elapsed().as_millis();

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok, "force get should return ok");
    let data = response.data.expect("force get should return data");

    // The provider was re-executed inline: the result comes from the real
    // hostname provider, not the seeded "stale-host" value.
    let name = data["name"]
        .as_str()
        .expect("hostname.name should be a string");
    assert!(
        !name.is_empty(),
        "hostname.name should not be empty after force re-execution"
    );
    assert_ne!(
        name, "stale-host",
        "force should have evicted the seeded value and re-executed"
    );

    // The answer is no older than the request: a fresh inline execution.
    let age = response.age_ms.expect("an age");
    assert!(
        age <= since_asked,
        "force re-execution should return a record run for it: {age}ms old, asked {since_asked}ms ago"
    );

    handle.abort();
}

/// force=false (default): a cached entry is returned as a hit.
/// This exercises the control path that force=true deviates from.
#[tokio::test]
async fn get_without_force_returns_cached_entry() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    let mut fields = HashMap::new();
    fields.insert("name".to_string(), Value::String("cached-host".to_string()));
    fields.insert("short".to_string(), Value::String("cached".to_string()));
    cache.put_source("hostname", None, "host", fields, Schedule::poll(60, 1));

    let server = Server::new(sock.clone(), Arc::clone(&cache), registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op":"get","key":"hostname"}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(response.ok);
    let data = response.data.expect("should return cached data");
    assert_eq!(
        data["name"].as_str(),
        Some("cached-host"),
        "without force, cache hit should return the seeded value"
    );

    handle.abort();
}

/// force=true on a static namespace must return an error without evicting the entry.
#[tokio::test]
async fn force_on_static_namespace_returns_error() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    // Register a static namespace via the put path (mirrors what Request::Put does).
    registry.register_static("mystore");

    let mut fields = HashMap::new();
    fields.insert("val".to_string(), Value::String("hello".to_string()));
    cache.put_source("mystore", None, PUT_SOURCE, fields, Schedule::default());

    let server = Server::new(sock.clone(), Arc::clone(&cache), registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op":"get","key":"mystore","force":true}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let response: Response = serde_json::from_str(&line).unwrap();
    assert!(
        !response.ok,
        "force on static namespace should return error"
    );
    assert!(
        response.error.as_deref().unwrap_or("").contains("static"),
        "error message should mention 'static', got: {:?}",
        response.error
    );

    // The stored entry must still be present — the guard ran before eviction.
    assert!(
        cache.get_entry("mystore", None).is_some(),
        "static entry must not be evicted by a failed force"
    );

    handle.abort();
}

/// wait field is accepted without error (no-op in this task; semantics land in T14).
#[tokio::test]
async fn get_with_wait_field_is_accepted() {
    let (_tmp, sock, cache, registry, watchers) = setup();

    let server = Server::new(sock.clone(), Arc::clone(&cache), registry, None, watchers);
    let handle = server.spawn().unwrap();

    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let request = r#"{"op":"get","key":"hostname","wait":true}"#;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let response: Response = serde_json::from_str(&line).unwrap();
    // Must not error out — wait is wired through but currently a no-op.
    assert!(response.ok, "wait=true should not cause a protocol error");

    handle.abort();
}

/// A Source that answers its run count, or fails while `fail` is set.
struct Flaky {
    meta: beachcomber::provider::SourceMetadata,
    runs: Arc<std::sync::atomic::AtomicU32>,
    fail: Arc<std::sync::atomic::AtomicBool>,
}

impl beachcomber::provider::Source for Flaky {
    fn metadata(&self) -> &beachcomber::provider::SourceMetadata {
        &self.meta
    }

    fn execute(&self, _: Option<&str>) -> Result<beachcomber::provider::SourceResult, String> {
        use std::sync::atomic::Ordering;
        let run = self.runs.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail.load(Ordering::SeqCst) {
            return Err("down".into());
        }
        let mut result = beachcomber::provider::SourceResult::new();
        result.insert("v", Value::Int(i64::from(run)));
        Ok(result)
    }
}

struct FlakyProvider(
    Arc<std::sync::atomic::AtomicU32>,
    Arc<std::sync::atomic::AtomicBool>,
);

fn flaky_meta() -> beachcomber::provider::SourceMetadata {
    use beachcomber::provider::{FailbackConfig, FieldSchema, FieldType, InvalidationStrategy};
    beachcomber::provider::SourceMetadata {
        name: "main".into(),
        fields: vec![FieldSchema {
            name: "v".into(),
            field_type: FieldType::Int,
        }],
        invalidation: InvalidationStrategy::Poll {
            interval_secs: 60,
            count: 2,
        },
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 30,
        },
        fsevents_reinstate: false,
        decay: None,
        budget: beachcomber::scheduler::lifecycle::DEFAULT_BUDGET,
        timeout: beachcomber::scheduler::lifecycle::DEFAULT_TIMEOUT,
        nearest: false,
    }
}

impl beachcomber::provider::Provider for FlakyProvider {
    fn metadata(&self) -> beachcomber::provider::ProviderMetadata {
        beachcomber::provider::ProviderMetadata {
            name: "flaky".into(),
            sources: vec![flaky_meta()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn beachcomber::provider::Source>> {
        vec![Box::new(Flaky {
            meta: flaky_meta(),
            runs: self.0.clone(),
            fail: self.1.clone(),
        })]
    }
}

/// `--force` always runs the Source, and a run that fails leaves the last good
/// value standing, as a run cut off at its timeout does: the forced read
/// answers it.
#[tokio::test]
async fn a_forced_run_that_fails_keeps_the_last_good_value() {
    use std::sync::atomic::Ordering;
    let (tmp, sock, cache, _, watchers) = setup();
    let runs = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let fail = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut registry = ProviderRegistry::new();
    registry
        .register(Box::new(FlakyProvider(runs.clone(), fail.clone())))
        .unwrap();
    let server = Server::new(sock.clone(), cache, Arc::new(registry), None, watchers);
    let handle = server.spawn().unwrap();

    let get = |force: bool| {
        let sock = sock.clone();
        async move {
            let mut stream = UnixStream::connect(&sock).await.unwrap();
            let request = serde_json::json!({"op": "get", "key": "flaky.v", "force": force});
            stream
                .write_all(format!("{request}\n").as_bytes())
                .await
                .unwrap();
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).await.unwrap();
            serde_json::from_str::<Response>(&line).unwrap()
        }
    };
    assert_eq!(get(false).await.data, Some(serde_json::json!(1)));
    fail.store(true, Ordering::SeqCst);
    let forced = get(true).await;
    assert_eq!(runs.load(Ordering::SeqCst), 2, "the forced read ran it");
    assert!(forced.ok, "{:?}", forced.error);
    assert_eq!(
        forced.data,
        Some(serde_json::json!(1)),
        "the failed run left the last good value"
    );
    drop(tmp);
    handle.abort();
}
