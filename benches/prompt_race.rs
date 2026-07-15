//! Prompt-race characterisation benchmarks.
//!
//! Measures, on a REAL host with the native filesystem watcher, how far the
//! async watch->refresh pipeline ("spend", L) lags behind the synchronous
//! post-mutation prompt read ("budget", B). See
//! docs/superpowers/specs/2026-06-14-prompt-race-characterisation-design.md.
//!
//! Measurements:
//!   * `fsevents_delivery` - the irreducible async floor: file write -> native
//!     watcher callback fires (steady-state, after the stream has started).
//!   * `convergence_L`     - end-to-end: `.git/HEAD` written -> cache reflects the
//!     new branch (FSEvents delivery + scheduler dispatch + refs execute + write).
//!
//! Budget B is NOT benched here - it is the prompt's read round-trip: ~5 ms via
//! the `comb` CLI (process spawn + socket), ~0.3 ms via an in-process SDK client
//! (see `benches/socket.rs::socket_roundtrip_cold`).
//!
//! macOS notes (verified via examples/fswatch_probe.rs):
//!   * FSEvents does NOT deliver events for `$TMPDIR` (`/var/folders/...`), so
//!     fixtures live under `$HOME` instead.
//!     (2026-07-15: did not reproduce against a healthy fseventsd —
//!     `/var/folders` delivered in ~10ms; the June observation was likely a
//!     symptom of the degraded fseventsd this branch was investigating. See
//!     tests/watch_probe_diag.rs. Fixture placement kept as-is: $HOME works
//!     in both worlds.)
//!   * FSEvents stream startup can take several seconds, so both benches warm the
//!     watcher until events actually flow before timing anything.
//!
//! If the watcher never delivers (sandbox/CI), the benches self-skip rather than
//! hang. Run on a real, unsandboxed host:
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
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::runtime::Runtime;
use tokio::sync::mpsc::Receiver;

/// Create a temp dir under `$HOME`, NOT `$TMPDIR`. On macOS, FSEvents does not
/// deliver events for `$TMPDIR` (`/var/folders/...`) - verified via
/// examples/fswatch_probe.rs (zero events there; events under `$HOME`). These
/// benches exist to measure native watch delivery, so the fixtures must live
/// where FSEvents actually reports.
fn fsevents_tempdir() -> TempDir {
    match std::env::var_os("HOME") {
        Some(home) => {
            let base = PathBuf::from(home).join(".cache").join("beachcomber-bench");
            std::fs::create_dir_all(&base).ok();
            TempDir::new_in(base).expect("create tempdir under $HOME/.cache")
        }
        None => TempDir::new().expect("create tempdir"),
    }
}

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

/// Point `.git/HEAD` at `refs/heads/<branch>` directly. This is the essential
/// on-disk change of a branch switch; doing it with a single file write (rather
/// than a `git checkout` subprocess) lets the caller take `t0` at the precise
/// instant the branch change hits disk, with no checkout-subprocess tail
/// overlapping the refresh pipeline. `branch` must already exist under
/// `.git/refs/heads/`.
fn set_head(repo: &str, branch: &str) {
    std::fs::write(
        Path::new(repo).join(".git").join("HEAD"),
        format!("ref: refs/heads/{branch}\n"),
    )
    .unwrap();
}

/// A throwaway git repo with two branches `a` and `b` at the same commit.
/// Checking out (or writing HEAD) between them rewrites `.git/HEAD` with no
/// working-tree churn - an isolated branch-change signal. Returns the `TempDir`
/// (keep it alive) and the repo root path. HEAD is left on branch `b`.
fn make_repo_with_branches() -> (TempDir, String) {
    let dir = fsevents_tempdir();
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
        if let Some((Value::String(s), _)) = cache.get_field("git", Some(path), "branch")
            && s == target
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_micros(200)).await;
    }
}

/// Warm a freshly-created native watcher until it actually delivers an event,
/// then drain the startup backlog. Returns false if nothing arrives within
/// `timeout` (sandboxed / non-delivering path). macOS FSEvents stream startup
/// can take several seconds; this absorbs that one-off latency before measuring.
async fn warm_until_delivering(
    dir: &Path,
    rx: &mut Receiver<Vec<PathBuf>>,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    let mut n = 0u64;
    while Instant::now() < deadline {
        std::fs::write(dir.join(format!("warm{n}")), b"x").unwrap();
        n += 1;
        if tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .is_ok()
        {
            while rx.try_recv().is_ok() {} // drain the startup backlog
            return true;
        }
    }
    false
}

/// Bench 1: irreducible async floor - time from a file write to the native
/// watcher delivering the corresponding event, in steady state.
fn bench_fsevents_delivery(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let dir = fsevents_tempdir();

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

    let delivering = rt.block_on(async {
        warm_until_delivering(dir.path(), &mut rx, Duration::from_secs(20)).await
    });
    if !delivering {
        eprintln!(
            "prompt_race: native watcher delivered nothing in 20s (sandbox, or a non-FSEvents path); skipping fsevents_delivery"
        );
        return;
    }

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut group = c.benchmark_group("fsevents_delivery");
    group.sample_size(10);
    group.bench_function("write_to_event", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
                while rx.try_recv().is_ok() {}
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
                    let f = dir.path().join(format!("f{n}"));
                    let t0 = Instant::now();
                    std::fs::write(&f, b"x").unwrap();
                    if tokio::time::timeout(Duration::from_secs(10), rx.recv())
                        .await
                        .is_err()
                    {
                        eprintln!(
                            "prompt_race: fsevents_delivery recv timed out (dropped/slow event); sample unreliable"
                        );
                    }
                    total += t0.elapsed();
                    while rx.try_recv().is_ok() {}
                }
                total
            })
        })
    });
    group.finish();
}

/// Bench 2: end-to-end convergence - time from a `.git/HEAD` write to the cache
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

    // Probe / warm-up: flip to "a" and wait up to 20s for the watch-driven refresh
    // to land. Absorbs FSEvents stream startup (several seconds on macOS). If it
    // never lands, the native watcher isn't delivering here - self-skip.
    let probe_ok = rt.block_on(async {
        set_head(&path, "a");
        wait_for_branch(&cache, &path, "a", Duration::from_secs(20)).await
    });
    if !probe_ok {
        eprintln!(
            "prompt_race: convergence probe failed in 20s (sandbox, or a non-FSEvents path); skipping convergence_L"
        );
        return;
    }

    let mut group = c.benchmark_group("convergence_L");
    group.sample_size(10);
    group.bench_function("head_to_cache", |b| {
        b.iter_custom(|iters| {
            rt.block_on(async {
                let mut total = Duration::ZERO;
                for i in 0..iters {
                    // Alternate target so each iteration is a real HEAD change.
                    // Probe left HEAD on "a", so i=0 flips to "b".
                    let target = if i % 2 == 0 { "b" } else { "a" };
                    // Time from the instant the branch change hits disk (the
                    // .git/HEAD write) to the cache reflecting it.
                    let t0 = Instant::now();
                    set_head(&path, target);
                    if !wait_for_branch(&cache, &path, target, Duration::from_secs(10)).await {
                        eprintln!(
                            "prompt_race: convergence_L iteration timed out (watcher not delivering live events?)"
                        );
                    }
                    total += t0.elapsed();
                }
                total
            })
        })
    });
    group.finish();
}

criterion_group!(benches, bench_fsevents_delivery, bench_convergence_l);
criterion_main!(benches);
