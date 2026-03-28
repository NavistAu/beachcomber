use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use beachcomber::cache::Cache;
use beachcomber::client::Client;
use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::scheduler::Scheduler;
use beachcomber::server::Server;
use std::sync::atomic::{AtomicU64, Ordering};
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
        rt.block_on(async { tokio::time::sleep(std::time::Duration::from_millis(200)).await });

        let sock_clone = sock.clone();
        let server = Server::new(sock_clone, cache, registry, Some(handle));
        rt.spawn(async move { server.run().await.unwrap() });
        rt.block_on(async { tokio::time::sleep(std::time::Duration::from_millis(50)).await });

        Self { _tmp: tmp, sock, rt }
    }
}

fn bench_concurrent_throughput(c: &mut Criterion) {
    let server = TestServer::new();

    let mut group = c.benchmark_group("concurrent_throughput");
    // Fewer samples since each iteration spawns many concurrent tasks.
    group.sample_size(20);

    for num_clients in [1usize, 10, 50, 100] {
        group.bench_with_input(
            BenchmarkId::from_parameter(num_clients),
            &num_clients,
            |b, &num_clients| {
                b.to_async(&server.rt).iter(|| {
                    let sock = server.sock.clone();
                    async move {
                        let counter = Arc::new(AtomicU64::new(0));
                        let mut handles = Vec::with_capacity(num_clients);

                        for _ in 0..num_clients {
                            let sock = sock.clone();
                            let counter = Arc::clone(&counter);
                            handles.push(tokio::spawn(async move {
                                let client = Client::new(sock);
                                // Each client does 10 gets.
                                for _ in 0..10 {
                                    if let Ok(resp) = client.get("hostname.name", None).await {
                                        if resp.ok {
                                            counter.fetch_add(1, Ordering::Relaxed);
                                        }
                                    }
                                }
                            }));
                        }

                        for h in handles {
                            h.await.unwrap();
                        }

                        let total = counter.load(Ordering::Relaxed);
                        criterion::black_box(total);
                    }
                })
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_concurrent_throughput);
criterion_main!(benches);
