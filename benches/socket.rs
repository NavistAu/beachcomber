use beachcomber::cache::Cache;
use beachcomber::client::Client;
use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::scheduler::Scheduler;
use beachcomber::server::Server;
use criterion::{Criterion, criterion_group, criterion_main};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::runtime::Runtime;

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
        let registry = Arc::new(ProviderRegistry::with_defaults());
        let config = Config::default();

        let (handle, scheduler) = Scheduler::new(cache.clone(), registry.clone(), config);
        rt.spawn(async move { scheduler.run().await });

        // Wait for scheduler to compute Once providers.
        rt.block_on(async { tokio::time::sleep(std::time::Duration::from_millis(200)).await });

        let sock_clone = sock.clone();
        let server = Server::new(sock_clone, cache, registry, Some(handle));
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
                let client = Client::new(sock);
                let resp = client.get("hostname.name", None).await.unwrap();
                criterion::black_box(resp);
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
                let client = Client::new(sock);
                let text = client.get_text("hostname.name", None).await.unwrap();
                criterion::black_box(text);
            }
        })
    });
}

/// Poke latency.
fn bench_socket_poke(c: &mut Criterion) {
    let server = TestServer::new();

    c.bench_function("socket_poke", |b| {
        b.to_async(&server.rt).iter(|| {
            let sock = server.sock.clone();
            async move {
                let client = Client::new(sock);
                let resp = client.poke("hostname", None).await.unwrap();
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

/// Persistent session: one connection, 10 sequential gets via ClientSession.
fn bench_socket_session_gets(c: &mut Criterion) {
    let server = TestServer::new();

    c.bench_function("session_10_gets", |b| {
        b.to_async(&server.rt).iter(|| {
            let sock = server.sock.clone();
            async move {
                let client = Client::new(sock);
                let mut session = client.connect().await.unwrap();
                for _ in 0..10 {
                    let resp = session.get("hostname.name", None).await.unwrap();
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
    bench_socket_poke,
    bench_sequential_gets,
    bench_socket_session_gets,
);
criterion_main!(benches);
