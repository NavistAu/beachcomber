//! Prompt-race characterisation benchmarks.
//!
//! Measures, on a REAL host with the native filesystem watcher, how far the
//! async watch->refresh pipeline ("spend", L) lags behind the synchronous
//! post-mutation prompt read ("budget", B). See
//! docs/superpowers/specs/2026-06-14-prompt-race-characterisation-design.md.
//!
//! Measurements:
//!   * `fsevents_delivery` - the irreducible async floor: file write -> native
//!     watcher callback fires.
//!   * `convergence_L`     - end-to-end: `.git/HEAD` change -> cache reflects the
//!     new branch (FSEvents delivery + scheduler dispatch + refs execute + write).
//!
//! Budget B is NOT benched here - it is the prompt's read round-trip: ~5 ms via
//! the `comb` CLI (process spawn + socket), ~0.3 ms via an in-process SDK client
//! (see `benches/socket.rs::socket_roundtrip_cold`). Either way B << L, so the
//! async watch path cannot win the race; these numbers quantify by how much.
//!
//! These benches REQUIRE a working native watcher. Under a sandbox that blocks
//! FSEvents/inotify they self-skip (print a message and return) rather than hang.
//! Run for real on an unsandboxed macOS and Linux host:
//!     cargo bench --bench prompt_race

use beachcomber::cache::Cache;
use beachcomber::config::Config;
use beachcomber::provider::Value;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::query::SourceDemand;
use beachcomber::scheduler::{Scheduler, SchedulerMessage};
use beachcomber::watcher::FsWatcher;
use beachcomber::watcher_registry::WatcherRegistry;
use criterion::{Criterion, criterion_group, criterion_main};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::runtime::Runtime;

/// Run a git subcommand in `dir`, panicking on failure. Bench-local because
/// benches cannot reach `tests/common`.
fn run_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn has_git() -> bool {
    Command::new("git").arg("--version").output().is_ok()
}

/// A throwaway git repo with two branches `a` and `b` at the same commit.
/// Checking out between them rewrites `.git/HEAD` with no working-tree churn -
/// an isolated branch-change signal. Returns the `TempDir` (keep it alive) and
/// the repo root path. HEAD is left on branch `b`.
fn make_repo_with_branches() -> (TempDir, String) {
    let dir = TempDir::new().unwrap();
    let p = dir.path();
    run_git(p, &["init"]);
    run_git(p, &["config", "user.email", "bench@bench.test"]);
    run_git(p, &["config", "user.name", "Bench"]);
    std::fs::write(p.join("README.md"), "# bench").unwrap();
    run_git(p, &["add", "."]);
    run_git(p, &["commit", "-m", "init"]);
    run_git(p, &["checkout", "-b", "a"]);
    run_git(p, &["checkout", "-b", "b"]);
    let path = p.to_str().unwrap().to_string();
    (dir, path)
}

/// Poll the cache until `git.branch` at `path` equals `target`, or `timeout`
/// elapses. Returns true on convergence. 200us poll granularity << L.
async fn wait_for_branch(cache: &Cache, path: &str, target: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some((Value::String(s), _)) = cache.get_field("git", Some(path), "branch") {
            if s == target {
                return true;
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_micros(200)).await;
    }
}

/// Bench 1: irreducible async floor - time from a file write to the native
/// watcher delivering the corresponding event.
fn bench_fsevents_delivery(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let dir = TempDir::new().unwrap();

    let (mut watcher, mut rx) = match FsWatcher::new() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("prompt_race: FsWatcher::new failed ({e}); skipping fsevents_delivery");
            return;
        }
    };
    if let Err(e) = watcher.watch(dir.path()) {
        eprintln!("prompt_race: watch failed ({e}); skipping fsevents_delivery");
        return;
    }

    // FSEvents/inotify streams take a moment to start delivering. Warm up, then
    // confirm a write produces an event within 2s - else self-skip (sandboxed).
    let delivering = rt.block_on(async {
        for i in 0..20 {
            std::fs::write(dir.path().join(format!("warm{i}")), b"x").unwrap();
            let _ = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await;
        }
        while rx.try_recv().is_ok() {}
        std::fs::write(dir.path().join("probe"), b"x").unwrap();
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .is_ok()
    });
    if !delivering {
        eprintln!("prompt_race: native watcher not delivering (sandboxed?); skipping fsevents_delivery");
        return;
    }

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    c.bench_function("fsevents_delivery", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
                while rx.try_recv().is_ok() {}
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
                    let f = dir.path().join(format!("f{n}"));
                    let t0 = Instant::now();
                    std::fs::write(&f, b"x").unwrap();
                    let _ = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await;
                    total += t0.elapsed();
                    while rx.try_recv().is_ok() {}
                }
                total
            })
        })
    });
}

/// Bench 2: end-to-end convergence - time from a `.git/HEAD` change to the cache
/// reflecting the new branch, driven by the real Scheduler + native watcher.
fn bench_convergence_l(c: &mut Criterion) {
    if !has_git() {
        eprintln!("prompt_race: git not found; skipping convergence_L");
        return;
    }
    let rt = Runtime::new().unwrap();
    let (_repo, path) = make_repo_with_branches();

    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let (handle, scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        Config::default(),
        Arc::new(WatcherRegistry::new()),
    );
    rt.spawn(async move { scheduler.run().await });

    // Warm: demand git at the repo root. Registers the native fs-watch on the
    // repo and inline-executes refs (cache branch = "b").
    rt.block_on(async {
        handle
            .send(SchedulerMessage::QueryActivity {
                provider: "git".to_string(),
                path: Some(path.clone()),
                demand: SourceDemand::All,
            })
            .await;
        tokio::time::sleep(Duration::from_millis(500)).await;
    });

    // Probe: flip to "a" and wait up to 3s for the watch-driven refresh to land.
    // If it never lands, the native watcher isn't delivering here - self-skip.
    let probe_ok = rt.block_on(async {
        run_git(Path::new(&path), &["checkout", "a"]);
        wait_for_branch(&cache, &path, "a", Duration::from_secs(3)).await
    });
    if !probe_ok {
        eprintln!("prompt_race: convergence probe failed (sandboxed watcher?); skipping convergence_L");
        return;
    }

    c.bench_function("convergence_L", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
                let mut total = Duration::ZERO;
                for i in 0..iters {
                    // Alternate target so each iteration is a real HEAD change.
                    // Probe left HEAD on "a", so i=0 flips to "b".
                    let target = if i % 2 == 0 { "b" } else { "a" };
                    // Mutation (UNTIMED): rewrite .git/HEAD.
                    let p = path.clone();
                    let t = target.to_string();
                    tokio::task::spawn_blocking(move || run_git(Path::new(&p), &["checkout", &t]))
                        .await
                        .unwrap();
                    // Measure: mutation -> cache reflects the new branch.
                    let t0 = Instant::now();
                    wait_for_branch(&cache, &path, target, Duration::from_secs(5)).await;
                    total += t0.elapsed();
                }
                total
            })
        })
    });
}

criterion_group!(benches, bench_fsevents_delivery, bench_convergence_l);
criterion_main!(benches);
