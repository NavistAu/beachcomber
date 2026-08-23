//! CLI process-invocation benchmarks.
//!
//! Every other bench in this suite measures the library path (cache, socket,
//! protocol, providers, throughput). None of them measure `comb` as a
//! process — the thing a shell prompt actually pays for. `benches/prompt_race.rs`
//! asserts a ~5ms figure for that cost (vs ~0.3ms for an in-process client) but
//! nothing here substantiates it. These benches close that gap.
//!
//! Measurements:
//!   * `cli_get_cold` - a full `comb get <key>` process invocation (spawn +
//!     connect + round-trip + exit) against an already-running daemon. This is
//!     the number `prompt_race.rs` claims is ~5ms.
//!   * `runtime_construction` - `tokio::runtime::Runtime::new()` (multi-thread,
//!     the builder every CLI command currently uses) immediately dropped, in
//!     isolation.
//!   * `runtime_construction_current_thread` - the same via
//!     `Builder::new_current_thread()`, to see whether the multi-thread pool
//!     (one worker per core, from `features = ["full"]`) is the expensive part.
//!
//! Together these answer: of the ~5ms a `comb get` invocation costs, how much
//! is the per-invocation tokio runtime that every CLI command builds just to
//! `block_on` a single sequential await chain?
//!
//! REQUIRES A RELEASE BUILD. A debug `comb` is several times slower than
//! release and would make `cli_get_cold` meaningless as a baseline for this
//! question. This bench builds `target/release/comb` itself (via `cargo build
//! --release --bin comb`) if it is missing, but does not rebuild an
//! already-present binary — run `cargo build --release` first if `comb`
//! source has changed since the last release build.
//!
//! Run with: `cargo bench --bench cli`

use beachcomber::cache::Cache;
use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::scheduler::Scheduler;
use beachcomber::server::Server;
use beachcomber::watcher_registry::WatcherRegistry;
use criterion::{Criterion, criterion_group, criterion_main};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::runtime::Runtime;

/// Path to the release `comb` binary, building it if it doesn't exist yet.
/// Never rebuilds an existing binary — see module docs.
fn release_binary_path() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = PathBuf::from(manifest_dir).join("target/release/comb");
    if !path.exists() {
        eprintln!(
            "cli bench: {path:?} not found, building it (cargo build --release --bin comb)..."
        );
        let status = Command::new(env!("CARGO"))
            .args(["build", "--release", "--bin", "comb"])
            .current_dir(manifest_dir)
            .status()
            .expect("spawn cargo build --release --bin comb");
        assert!(status.success(), "cargo build --release --bin comb failed");
    }
    assert!(
        path.exists(),
        "release binary still missing after build: {path:?}"
    );
    path
}

/// An already-running daemon, in-process, on an isolated socket. Same setup
/// as `benches/socket.rs::TestServer` / `benches/throughput.rs::TestServer` -
/// the daemon must be up before iteration starts so `cli_get_cold` measures
/// process invocation, not daemon startup.
struct TestServer {
    _tmp: TempDir,
    sock: std::path::PathBuf,
    _rt: Runtime,
}

impl TestServer {
    fn new() -> Self {
        let rt = Runtime::new().unwrap();
        let tmp = TempDir::new().unwrap();
        let sock = tmp.path().join("bench.sock");

        let cache = Arc::new(Cache::new());
        let registry = Arc::new(ProviderRegistry::with_defaults());
        let config = Config::default();

        let (handle, scheduler) = Scheduler::new(
            cache.clone(),
            registry.clone(),
            config,
            Arc::new(WatcherRegistry::new()),
        );
        rt.spawn(async move { scheduler.run().await });
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
        rt.block_on(async { tokio::time::sleep(std::time::Duration::from_millis(50)).await });

        Self {
            _tmp: tmp,
            sock,
            _rt: rt,
        }
    }
}

/// Full `comb get hostname.name` process invocation against a daemon that is
/// already up. This is the ~5ms figure `prompt_race.rs` claims for CLI budget.
fn bench_cli_get_cold(c: &mut Criterion) {
    let binary = release_binary_path();
    let server = TestServer::new();

    c.bench_function("cli_get_cold", |b| {
        b.iter(|| {
            let output = Command::new(&binary)
                .args(["get", "hostname.name"])
                .env("BEACHCOMBER_SOCKET", &server.sock)
                .env("RUST_LOG", "error")
                .current_dir("/")
                .output()
                .expect("spawn comb get");
            assert!(
                output.status.success(),
                "comb get failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            criterion::black_box(output);
        })
    });
}

/// `tokio::runtime::Runtime::new()` (multi-thread, `enable_all` by default)
/// immediately dropped. Isolates exactly what six CLI commands each build and
/// throw away once per invocation.
fn bench_runtime_construction(c: &mut Criterion) {
    c.bench_function("runtime_construction", |b| {
        b.iter(|| {
            let rt = Runtime::new().expect("Runtime::new");
            drop(rt);
        })
    });
}

/// The current-thread builder for comparison, to see whether the multi-thread
/// worker pool (one thread per core) is the expensive part of construction.
fn bench_runtime_construction_current_thread(c: &mut Criterion) {
    c.bench_function("runtime_construction_current_thread", |b| {
        b.iter(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("current-thread Runtime::build");
            drop(rt);
        })
    });
}

criterion_group!(
    benches,
    bench_cli_get_cold,
    bench_runtime_construction,
    bench_runtime_construction_current_thread,
);
criterion_main!(benches);
