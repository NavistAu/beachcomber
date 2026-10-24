use beachcomber::cache::Cache;
use beachcomber::client::Client;
use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::scheduler::Scheduler;
use beachcomber::server::Server;
use beachcomber::watcher_registry::WatcherRegistry;
use criterion::{Criterion, criterion_group, criterion_main};
use libbeachcomber::resolve::{Inputs, Slot, resolve};
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::runtime::Runtime;

/// A `libbeachcomber` client on `sock`, with no daemon auto-start — for the
/// benches that measure its own API (`get_text`, a `Session`).
fn lib_client(sock: std::path::PathBuf) -> libbeachcomber::Client {
    libbeachcomber::Client::with_config(libbeachcomber::ClientConfig {
        timeout: std::time::Duration::from_secs(2),
        auto_start: false,
        exec: false,
    })
    .with_socket_path(sock)
}

struct TestServer {
    _tmp: TempDir,
    sock: std::path::PathBuf,
    rt: Runtime,
}

impl TestServer {
    fn new() -> Self {
        let rt = Runtime::new().unwrap();
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("bench.sock");

        let cache = Arc::new(Cache::new());
        let registry = Arc::new(shipped_registry());
        let config = Config::default();

        let (handle, scheduler) = Scheduler::new(
            cache.clone(),
            registry.clone(),
            config,
            Arc::new(WatcherRegistry::new()),
        );
        rt.spawn(async move { scheduler.run().await });

        // Wait for scheduler to compute Once providers.
        rt.block_on(async { tokio::time::sleep(std::time::Duration::from_millis(200)).await });

        let sock_clone = sock.clone();
        let server = Server::new(
            sock_clone,
            cache,
            registry,
            Some(handle),
            Arc::new(WatcherRegistry::new()),
        );
        rt.spawn(async move { server.run().await.unwrap() });

        // Wait for the server socket to appear.
        rt.block_on(async { tokio::time::sleep(std::time::Duration::from_millis(50)).await });

        Self {
            _tmp: tmp,
            sock,
            rt,
        }
    }
}

/// Cold connection: each iteration opens a new connection, sends one get, closes.
fn bench_socket_roundtrip_cold(c: &mut Criterion) {
    let server = TestServer::new();

    c.bench_function("socket_roundtrip_cold", |b| {
        b.to_async(&server.rt).iter(|| {
            let sock = server.sock.clone();
            async move {
                // The path `comb get` takes: a session, then the resolver.
                let mut session = Client::new(sock).connect().unwrap();
                let inputs = Inputs {
                    cwd: "/",
                    env: &HashMap::new(),
                    slot: Slot::Global,
                    force: false,
                    wait: false,
                    allow_exec: true,
                };
                let value = resolve("hostname.name", &inputs, &mut session).unwrap();
                criterion::black_box(value);
            }
        })
    });
}

/// Text format round-trip (same cold connection pattern, text response).
fn bench_socket_roundtrip_text(c: &mut Criterion) {
    let server = TestServer::new();

    c.bench_function("socket_roundtrip_text", |b| {
        b.to_async(&server.rt).iter(|| {
            let sock = server.sock.clone();
            async move {
                let text = lib_client(sock).get_text("hostname.name", None).unwrap();
                criterion::black_box(text);
            }
        })
    });
}

/// Refresh latency.
fn bench_socket_refresh(c: &mut Criterion) {
    let server = TestServer::new();

    c.bench_function("socket_refresh", |b| {
        b.to_async(&server.rt).iter(|| {
            let sock = server.sock.clone();
            async move {
                let client = Client::new(sock);
                let resp = client.refresh("hostname", None).unwrap();
                criterion::black_box(resp);
            }
        })
    });
}

/// Sequential gets on the same persistent connection.
fn bench_sequential_gets(c: &mut Criterion) {
    let server = TestServer::new();

    c.bench_function("sequential_100_gets", |b| {
        b.to_async(&server.rt).iter(|| {
            let sock = server.sock.clone();
            async move {
                use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
                use tokio::net::UnixStream;

                let stream = UnixStream::connect(&sock).await.unwrap();
                let (reader, mut writer) = stream.into_split();
                let mut reader = BufReader::new(reader);

                for _ in 0..100 {
                    let req = b"{\"op\":\"get\",\"key\":\"hostname.name\"}\n";
                    writer.write_all(req).await.unwrap();
                    let mut line = String::new();
                    reader.read_line(&mut line).await.unwrap();
                    criterion::black_box(&line);
                    line.clear();
                }
            }
        })
    });
}

/// Persistent session: one connection, 10 sequential gets via `Session`.
fn bench_socket_session_gets(c: &mut Criterion) {
    let server = TestServer::new();

    c.bench_function("session_10_gets", |b| {
        b.to_async(&server.rt).iter(|| {
            let sock = server.sock.clone();
            async move {
                let mut session = lib_client(sock).session().unwrap();
                for _ in 0..10 {
                    let resp = session.get("hostname.name", None).unwrap();
                    criterion::black_box(resp);
                }
            }
        })
    });
}

criterion_group!(
    benches,
    bench_socket_roundtrip_cold,
    bench_socket_roundtrip_text,
    bench_socket_refresh,
    bench_sequential_gets,
    bench_socket_session_gets,
);
criterion_main!(benches);

/// The built-in providers with the shipped `defaults/00-comb.toml` composed in,
/// as a running daemon has them: the abstract provider's lines are part of
/// what a read costs.
fn shipped_registry() -> ProviderRegistry {
    let config = Config::from_layers(vec![beachcomber::daemon::defaults::comb_layer()]);
    ProviderRegistry::with_config(&config, &Default::default()).expect("the shipped lines load")
}
