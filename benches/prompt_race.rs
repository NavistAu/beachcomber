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
//!   * `read_after_mutation` (printed, not criterion) - correctness: a change on
//!     disk, then at once a `get` over the raw socket, counted right when it
//!     answers the new value, with the read's own latency. Two changes: a
//!     `.git/HEAD` write read as `git.branch`, and a `state.json` rewrite read
//!     through a config Source that declares `watch('state.json')`. Run with
//!     the watcher on (each trial starts once it has caught up with the last
//!     one) and with it off (a server with no scheduler: the read's own check
//!     of the record's files is all there is).
//!
//!   * `memo_after_rewrite` (printed, not criterion) - T5.5: a memoised template
//!     leaf (`cat <file>`, watching the file) read by the library client, a hit,
//!     then the same command uncached (the spawn), then the file rewritten and
//!     the leaf read at once. The first read after the rewrite is counted right
//!     when it answers the new content, and costs what a hit and a spawn cost
//!     plus the difference reported.
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
use beachcomber::query::Instance;
use beachcomber::scheduler::{Scheduler, SchedulerMessage};
use beachcomber::server::Server;
use beachcomber::watcher::FsWatcher;
use beachcomber::watcher_registry::WatcherRegistry;
use criterion::{Criterion, criterion_group};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
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
    let registry = Arc::new(ProviderRegistry::shipped(&std::env::vars().collect()));
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
                instances: vec![
                    Instance {
                        source: "head".to_string(),
                        root: Some(path.clone()),
                    },
                    Instance {
                        source: "refs".to_string(),
                        root: Some(path.clone()),
                    },
                    Instance {
                        source: "diff".to_string(),
                        root: Some(path.clone()),
                    },
                    Instance {
                        source: "lineage".to_string(),
                        root: Some(path.clone()),
                    },
                    Instance {
                        source: "status".to_string(),
                        root: Some(path.clone()),
                    },
                ],
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

/// Trials per change in `read_after_mutation`.
const READ_TRIALS: usize = 100;

/// One request on a raw connection, its answer and how long it took.
fn timed_get(stream: &mut UnixStream, key: &str, path: &str) -> (serde_json::Value, Duration) {
    let mut line =
        serde_json::to_string(&serde_json::json!({"op": "get", "key": key, "path": path})).unwrap();
    line.push('\n');
    let t0 = Instant::now();
    stream.write_all(line.as_bytes()).unwrap();
    let mut answer = String::new();
    BufReader::new(&mut *stream).read_line(&mut answer).unwrap();
    let took = t0.elapsed();
    (serde_json::from_str(&answer).expect("a JSON answer"), took)
}

/// Poll the cache until `field` of `provider` at `path` reads `want`.
async fn wait_for_field(
    cache: &Cache,
    provider: &str,
    path: &str,
    field: &str,
    want: &Value,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if cache
            .get_field(provider, Some(path), field)
            .is_some_and(|(v, _)| v == *want)
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_micros(200)).await;
    }
}

/// The count a read after a change gets right, and the read's latency, with
/// the watcher on and with it off.
fn read_after_mutation() {
    if !has_git() {
        eprintln!("prompt_race: git not found; skipping read_after_mutation");
        return;
    }
    let rt = Runtime::new().unwrap();
    let (_repo, repo) = make_repo_with_branches();
    let state_dir = fsevents_tempdir();
    let state = state_dir
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    std::fs::write(Path::new(&state).join(".rstate"), "").unwrap();
    std::fs::write(Path::new(&state).join("state.json"), "start").unwrap();

    let layer: toml::Value = toml::from_str(
        "[daemon.providers.rstate.sources.s]\n\
         root = \"{{ lup('.rstate') | dirname }}\"\n\
         data = \"{{ 'state.json' | read | trim }}\"\n\
         result.v = \"{{ data }}\"\n\
         return = \"{{ result | watch('state.json') }}\"\n",
    )
    .unwrap();
    let mut layers = beachcomber::daemon::defaults::shipped_layers();
    layers.push((PathBuf::from("<bench>"), layer));
    let config = Config::from_layers(layers);
    let serve = |watched: bool| {
        let watchers = Arc::new(WatcherRegistry::new());
        let cache = Arc::new(Cache::with_watchers(watchers.clone()));
        let registry = ProviderRegistry::with_config(&config, &std::env::vars().collect())
            .expect("the bench lines load")
            .share(&cache);
        let handle = watched.then(|| {
            let (handle, scheduler) = Scheduler::new(
                cache.clone(),
                registry.clone(),
                config.clone(),
                watchers.clone(),
            );
            rt.spawn(async move { scheduler.run().await });
            handle
        });
        let sock_dir = TempDir::new().unwrap();
        let sock = sock_dir.path().join("s");
        let server = Server::new(sock.clone(), cache.clone(), registry, handle, watchers);
        {
            let _enter = rt.enter();
            server.spawn().expect("bind the bench socket");
        }
        let stream = UnixStream::connect(&sock).unwrap();
        (sock_dir, cache, stream)
    };

    // With the watcher off first: nothing runs but what a read starts.
    {
        let (_sock, _cache, mut stream) = serve(false);
        for (key, path) in [("git.branch", &repo), ("rstate.v", &state)] {
            let (answer, _) = timed_get(&mut stream, key, path);
            assert!(answer.get("data").is_some(), "{key} answers: {answer}");
        }
        println!("read_after_mutation: {READ_TRIALS} trials each, watcher off");
        trials(&mut stream, &repo, &state, |_| {});
    }

    let (_sock, cache, mut stream) = serve(true);
    // The cold reads demand both instances, which registers their watches. A
    // change made before a registration is in place is never reported, so the
    // probe changes the file until the watcher carries one into the cache.
    for (key, path) in [("git.branch", &repo), ("rstate.v", &state)] {
        let (answer, _) = timed_get(&mut stream, key, path);
        assert!(answer.get("data").is_some(), "{key} answers: {answer}");
    }
    let delivering = rt.block_on(async {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut n = 0u32;
        while Instant::now() < deadline {
            let probe = format!("probe{n}");
            n += 1;
            std::fs::write(Path::new(&state).join("state.json"), &probe).unwrap();
            let seen = Value::String(probe);
            let wait = Duration::from_millis(500);
            if wait_for_field(&cache, "rstate", &state, "v", &seen, wait).await {
                return true;
            }
        }
        false
    });
    if !delivering {
        eprintln!(
            "prompt_race: the watcher delivered nothing in 20s (sandbox, or a non-FSEvents path); skipping read_after_mutation with the watcher on"
        );
        return;
    }
    println!("read_after_mutation: {READ_TRIALS} trials each, watcher on");
    // Each trial starts once the watcher has caught up with the last one.
    trials(&mut stream, &repo, &state, |trial| {
        rt.block_on(async {
            match trial {
                Trial::Branch(target) => {
                    wait_for_branch(&cache, &repo, target, Duration::from_secs(10)).await;
                }
                Trial::State(want) => {
                    let want = Value::String(want.to_string());
                    wait_for_field(
                        &cache,
                        "rstate",
                        &state,
                        "v",
                        &want,
                        Duration::from_secs(10),
                    )
                    .await;
                }
            }
        })
    });
}

/// How many reads each memo measurement takes.
const MEMO_TRIALS: usize = 100;

/// A daemon in this process serving the memoised leaf `p.cached`, the same
/// command uncached as `p.spawn`, and the file both read.
struct MemoFixture {
    rt: Option<Runtime>,
    cancel: tokio_util::sync::CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
    sock: PathBuf,
    /// The configuration and the socket live here.
    _conf: TempDir,
    /// The file the leaves `cat`, and the memo watches.
    file: PathBuf,
    _kube: TempDir,
}

impl MemoFixture {
    fn start() -> Self {
        let rt = Runtime::new().unwrap();
        let conf = TempDir::new().unwrap();
        let kube = fsevents_tempdir();
        let file = kube.path().canonicalize().unwrap().join("kubeconfig");
        std::fs::write(&file, "start").unwrap();
        let file_str = file.to_string_lossy().into_owned();
        let xdg = conf.path().join("xdg");
        let config_file = xdg.join("beachcomber").join("config.toml");
        let defaults = xdg.join("beachcomber").join("defaults");
        std::fs::create_dir_all(&defaults).unwrap();
        beachcomber::daemon::defaults::write_managed_defaults(&defaults, env!("CARGO_PKG_VERSION"))
            .expect("write the defaults");
        std::fs::write(
            &config_file,
            format!(
                "[client.providers.p]\n\
                 template.cached = \"{{{{ 'cat {file_str}' | watch('{file_str}') | ttl('1h') | cache() | exec() }}}}\"\n\
                 template.spawn = \"{{{{ 'cat {file_str}' | exec() }}}}\"\n"
            ),
        )
        .unwrap();
        let config = Config::load_from(&config_file);
        assert!(config.errors().is_empty(), "{:?}", config.errors());
        let sock = conf.path().join("s");
        let cancel = tokio_util::sync::CancellationToken::new();
        let (task, ready) = {
            let _enter = rt.enter();
            beachcomber::daemon::start_in_process_with_cancel(sock.clone(), config, cancel.clone())
        };
        rt.block_on(ready).expect("the bench daemon starts");
        Self {
            rt: Some(rt),
            cancel,
            task: Some(task),
            sock,
            _conf: conf,
            file,
            _kube: kube,
        }
    }

    fn client(&self) -> libbeachcomber::Client {
        libbeachcomber::Client::with_config(libbeachcomber::ClientConfig {
            auto_start: false,
            exec: true,
            timeout: Duration::from_secs(5),
        })
        .with_socket_path(self.sock.clone())
    }

    fn rewrite(&self, content: &str) {
        std::fs::write(&self.file, content).unwrap();
    }
}

impl Drop for MemoFixture {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let (Some(rt), Some(task)) = (self.rt.take(), self.task.take()) {
            let _ = rt.block_on(task);
            rt.shutdown_background();
        }
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[(v.len() - 1) / 2]
}

/// T5.5: the first read of a memoised leaf after its watched file was
/// rewritten, against a hit and a spawn, over the library client.
fn memo_after_rewrite() {
    let fx = MemoFixture::start();
    let client = fx.client();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let read = |key: &str| {
        let t0 = Instant::now();
        let got = client.get(key, None);
        (got, t0.elapsed())
    };
    for key in ["p.cached", "p.cached", "p.spawn"] {
        let (got, _) = read(key);
        assert!(matches!(got, Ok(Some(_))), "{key} answers: {got:?}");
    }
    let (mut right, mut hits, mut spawns, mut misses) = (0, vec![], vec![], vec![]);
    for i in 0..MEMO_TRIALS {
        let want = format!("v{i}");
        let (_, hit) = read("p.cached");
        let (_, spawn) = read("p.spawn");
        fx.rewrite(&want);
        let (got, miss) = read("p.cached");
        right += usize::from(matches!(&got, Ok(Some(v)) if *v == serde_json::json!(want)));
        hits.push(ms(hit));
        spawns.push(ms(spawn));
        misses.push(ms(miss));
    }
    let over: Vec<f64> = (0..MEMO_TRIALS)
        .map(|i| misses[i] - hits[i] - spawns[i])
        .collect();
    println!("memo_after_rewrite: {MEMO_TRIALS} trials, watcher on");
    println!(
        "  {right}/{MEMO_TRIALS} answered the new content; p50 hit {:.3}ms, spawn {:.3}ms, read after rewrite {:.3}ms; miss - hit - spawn: p50 {:.3}ms",
        median(hits),
        median(spawns),
        median(misses),
        median(over)
    );
}

/// The change one trial made, for the wait after it.
enum Trial<'a> {
    Branch(&'a str),
    State(&'a str),
}

/// `READ_TRIALS` of each change, each read at once, then `after` the trial.
fn trials(stream: &mut UnixStream, repo: &str, state: &str, after: impl Fn(Trial)) {
    let report = |what: &str, right: usize, mut reads: Vec<Duration>| {
        reads.sort();
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let at = |q: f64| reads[((reads.len() - 1) as f64 * q).round() as usize];
        println!(
            "  {what}: {right}/{READ_TRIALS} answered the new value; read p50 {:.3}ms p99 {:.3}ms max {:.3}ms",
            ms(at(0.5)),
            ms(at(0.99)),
            ms(reads[reads.len() - 1])
        );
    };

    let (mut right, mut reads) = (0, Vec::new());
    for i in 0..READ_TRIALS {
        let target = if i % 2 == 0 { "a" } else { "b" };
        set_head(repo, target);
        let (answer, took) = timed_get(stream, "git.branch", repo);
        right += usize::from(answer["data"] == serde_json::json!(target));
        reads.push(took);
        after(Trial::Branch(target));
    }
    report("git.branch after a .git/HEAD write", right, reads);

    let (mut right, mut reads) = (0, Vec::new());
    for i in 0..READ_TRIALS {
        let want = format!("v{i}");
        std::fs::write(Path::new(state).join("state.json"), &want).unwrap();
        let (answer, took) = timed_get(stream, "rstate.v", state);
        right += usize::from(answer["data"] == serde_json::json!(want));
        reads.push(took);
        after(Trial::State(&want));
    }
    report("rstate.v after a state.json rewrite", right, reads);
}

criterion_group!(benches, bench_fsevents_delivery, bench_convergence_l);

fn main() {
    // A filter is the first argument that is not a flag (`cargo bench` passes
    // `--bench`); with none, every part runs.
    let filter = std::env::args().skip(1).find(|a| !a.starts_with('-'));
    let runs = |name: &str| filter.as_deref().is_none_or(|f| name.contains(f));
    if runs("fsevents_delivery") || runs("convergence_L") {
        benches();
        Criterion::default().configure_from_args().final_summary();
    }
    if runs("read_after_mutation") {
        read_after_mutation();
    }
    if runs("memo_after_rewrite") {
        memo_after_rewrite();
    }
}
