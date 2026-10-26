//! Read-validity baselines (phase 4 of the one-tree rebuild).
//!
//! Every measurement is over the raw socket — NDJSON on a `UnixStream`, no
//! client library — against the daemon's own startup path run in-process
//! (`daemon::start_in_process_with_cancel`): the native file watcher and its
//! self-test, on a multi-thread runtime of one worker per core as `comb
//! daemon` builds it, serving the shipped defaults plus the Sources below.
//!
//! Criterion groups, each on one connection held open:
//!   * `warm_get/watch0`, `watch1`, `watch8`: a warm `get` of a rooted config
//!     Source that declares 0 (it only polls), 1 and 8 `watch` paths;
//!   * `warm_get/git.branch`, `warm_get/git`: a field and the whole namespace
//!     on a fixture repository.
//!
//! Printed:
//!   * `warm_get_percentiles`: the same reads interleaved, one of each case per
//!     round, as min, p50, p90 and p99. On a loaded machine criterion's means
//!     swing with the load from one case to the next; interleaving shifts every
//!     case alike, and the targets are written as percentiles.
//!   * `budget_accuracy`: a 300 ms Source read once its record is past its
//!     lifetime: the default read against its budget, and a read with `wait`
//!     against the run (target T4.4).
//!   * `second_get_after_cold`: in each of a number of fresh daemons, the cold
//!     `get` of a `root = lup(…)` plus `watch(…)` Source, the next `get` of it on
//!     the same connection, a third, and then an `introspect watches`, which
//!     the scheduler loop answers (audit finding P9). Each sample costs a daemon
//!     start, which a criterion iteration cannot hold.
//!
//! Run with `cargo bench --bench validity`. A filter runs one part:
//! `-- warm_get/` the criterion groups, `-- warm_get_percentiles`,
//! `-- second_get_after_cold` or `-- budget_accuracy` a printed one.

use beachcomber::config::Config;
use criterion::{Criterion, criterion_group};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

/// How many fresh daemons `second_get_after_cold` starts.
const FRESH_DAEMONS: usize = 12;

/// The files the `watch` Sources name, under the project directory.
const WATCHED: [&str; 8] = ["f0", "f1", "f2", "f3", "f4", "f5", "f6", "f7"];

/// A config Source of provider `name`, rooted at the directory holding
/// `marker`, whose `return` decorates its result with `ret`.
fn rooted(name: &str, marker: &str, ret: &str) -> String {
    format!(
        "[daemon.providers.{name}.sources.s]\n\
         root = \"{{{{ lup('{marker}') | dirname }}}}\"\n\
         data = \"{{{{ 1 }}}}\"\n\
         result.n = \"{{{{ data }}}}\"\n\
         return = \"{{{{ result | {ret} }}}}\"\n"
    )
}

fn sources() -> String {
    let eight: Vec<String> = WATCHED.iter().map(|f| format!("'{f}'")).collect();
    [
        rooted("watch0", ".vmark", "poll('10m')"),
        rooted("watch1", ".vmark", "watch('f0')"),
        rooted("watch8", ".vmark", &format!("watch({})", eight.join(", "))),
        rooted("p9", ".p9", "watch('state.json')"),
    ]
    .join("\n")
}

/// The directories the reads address: a project the config Sources root at,
/// and a git repository.
struct Fixture {
    _dir: TempDir,
    proj: PathBuf,
    p9: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        // The daemon canonicalises a requested path; so does this, so the
        // path a read names is the one it is stored at.
        let base = dir.path().canonicalize().expect("canonical tempdir");
        let proj = base.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join(".vmark"), "").unwrap();
        for f in WATCHED {
            std::fs::write(proj.join(f), f).unwrap();
        }
        let p9 = base.join("p9");
        std::fs::create_dir_all(&p9).unwrap();
        std::fs::write(p9.join(".p9"), "").unwrap();
        std::fs::write(p9.join("state.json"), "{}").unwrap();
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "bench@bench.test"]);
        git(&repo, &["config", "user.name", "Bench"]);
        std::fs::write(repo.join("README.md"), "# bench").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        Self {
            _dir: dir,
            proj,
            p9,
            repo,
        }
    }
}

/// Run git in `dir`, whatever `GIT_DIR` the bench was started under says.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A daemon on its own runtime, as `comb daemon` builds it, stopped when
/// dropped.
struct Daemon {
    rt: Option<tokio::runtime::Runtime>,
    cancel: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
    sock: PathBuf,
    _dir: TempDir,
}

impl Daemon {
    fn start() -> Self {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let dir = TempDir::new().expect("socket dir");
        let sock = dir.path().join("s");
        let layer: toml::Value = toml::from_str(&sources()).expect("bench sources parse");
        let config = Config::from_layers(vec![
            beachcomber::daemon::defaults::comb_layer(),
            (PathBuf::from("<bench>"), layer),
        ]);
        let cancel = CancellationToken::new();
        let (task, ready) = {
            let _enter = rt.enter();
            beachcomber::daemon::start_in_process_with_cancel(sock.clone(), config, cancel.clone())
        };
        rt.block_on(ready)
            .expect("the bench daemon refused to start");
        Self {
            rt: Some(rt),
            cancel,
            task: Some(task),
            sock,
            _dir: dir,
        }
    }

    fn connect(&self) -> Conn {
        let stream = UnixStream::connect(&self.sock).expect("connect");
        Conn {
            reader: BufReader::new(stream.try_clone().expect("clone stream")),
            writer: stream,
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let (Some(rt), Some(task)) = (self.rt.take(), self.task.take()) {
            let _ = rt.block_on(task);
            rt.shutdown_background();
        }
    }
}

/// One connection, one request at a time.
struct Conn {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Conn {
    fn send(&mut self, request: &Value) -> Value {
        let mut line = serde_json::to_string(request).unwrap();
        line.push('\n');
        self.writer.write_all(line.as_bytes()).unwrap();
        let mut answer = String::new();
        self.reader.read_line(&mut answer).unwrap();
        serde_json::from_str(&answer).expect("a JSON answer")
    }

    fn get(&mut self, key: &str, path: &Path) -> Value {
        self.send(&json!({"op": "get", "key": key, "path": path}))
    }
}

/// The warm reads: `(name, key, directory)`.
fn warm_cases(fixture: &Fixture) -> [(&'static str, &'static str, &Path); 5] {
    [
        ("watch0", "watch0.n", &fixture.proj),
        ("watch1", "watch1.n", &fixture.proj),
        ("watch8", "watch8.n", &fixture.proj),
        ("git.branch", "git.branch", &fixture.repo),
        ("git", "git", &fixture.repo),
    ]
}

/// A daemon with every warm case read once, its Sources run and the
/// registrations their first demands started given time to finish.
fn warmed(fixture: &Fixture) -> (Daemon, Conn) {
    let daemon = Daemon::start();
    let mut conn = daemon.connect();
    for (_, key, path) in warm_cases(fixture) {
        let first = conn.get(key, path);
        assert!(first.get("data").is_some(), "{key} answers: {first}");
    }
    std::thread::sleep(Duration::from_secs(1));
    (daemon, conn)
}

/// The warm reads, each on one connection, after a first read has run the
/// Source and the daemon has settled.
fn bench_warm_gets(c: &mut Criterion) {
    let fixture = Fixture::new();
    let (_daemon, mut conn) = warmed(&fixture);
    let cases = warm_cases(&fixture);
    let mut group = c.benchmark_group("warm_get");
    for (name, key, path) in cases {
        group.bench_function(name, |b| {
            b.iter(|| criterion::black_box(conn.get(key, path)));
        });
    }
    group.finish();
}

/// Rounds of `warm_get_percentiles`.
const ROUNDS: usize = 2000;

/// The warm reads again, interleaved: each round reads every case once, so a
/// machine whose load swings shifts every case alike, and the report is the
/// percentiles the targets are written in.
fn warm_get_percentiles() {
    let fixture = Fixture::new();
    let (_daemon, mut conn) = warmed(&fixture);
    let cases = warm_cases(&fixture);
    let mut samples: Vec<Vec<Duration>> = vec![Vec::with_capacity(ROUNDS); cases.len()];
    for _ in 0..ROUNDS {
        for (i, (_, key, path)) in cases.iter().enumerate() {
            let t0 = Instant::now();
            criterion::black_box(conn.get(key, path));
            samples[i].push(t0.elapsed());
        }
    }
    let us = |d: Duration| d.as_secs_f64() * 1e6;
    println!("warm_get_percentiles: {ROUNDS} interleaved rounds, one connection");
    println!(
        "  {:<12} {:>9} {:>9} {:>9} {:>9}",
        "case", "min", "p50", "p90", "p99"
    );
    for ((name, _, _), mut taken) in cases.iter().zip(samples) {
        taken.sort();
        let at = |q: f64| taken[((taken.len() - 1) as f64 * q).round() as usize];
        println!(
            "  {name:<12} {:>7.1}µs {:>7.1}µs {:>7.1}µs {:>7.1}µs",
            us(taken[0]),
            us(at(0.5)),
            us(at(0.9)),
            us(at(0.99))
        );
    }
}

/// Audit P9: the second `get` right after a cold run, in fresh daemons.
fn second_get_after_cold() {
    let fixture = Fixture::new();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    println!(
        "second_get_after_cold: p9.n (root = lup(...), watch('state.json')), {FRESH_DAEMONS} fresh daemons"
    );
    println!(
        "  daemon   cold get   2nd get   3rd get   then introspect watches (asks the scheduler loop)"
    );
    let mut seconds = Vec::new();
    for i in 0..FRESH_DAEMONS {
        let daemon = Daemon::start();
        let mut conn = daemon.connect();
        let timed = |conn: &mut Conn| {
            let t0 = Instant::now();
            let answer = conn.get("p9.n", &fixture.p9);
            let took = t0.elapsed();
            assert_eq!(answer["data"], json!(1), "p9.n answers: {answer}");
            took
        };
        let cold = timed(&mut conn);
        let second = timed(&mut conn);
        let third = timed(&mut conn);
        let t0 = Instant::now();
        conn.send(&json!({"op": "introspect", "subject": "watches"}));
        let introspect = t0.elapsed();
        println!(
            "  {:>6}   {:>7.3}ms {:>7.3}ms {:>7.3}ms   {:>8.3}ms",
            i + 1,
            ms(cold),
            ms(second),
            ms(third),
            ms(introspect)
        );
        seconds.push(second);
    }
    seconds.sort();
    let at = |q: f64| seconds[((seconds.len() - 1) as f64 * q).round() as usize];
    println!(
        "  2nd get: min {:.3}ms  p50 {:.3}ms  max {:.3}ms; {} of {} under 1ms",
        ms(seconds[0]),
        ms(at(0.5)),
        ms(seconds[seconds.len() - 1]),
        seconds
            .iter()
            .filter(|d| **d < Duration::from_millis(1))
            .count(),
        seconds.len()
    );
}

/// Rounds of `budget_accuracy`.
const BUDGET_ROUNDS: usize = 4;

/// The `[daemon] budget` every Source here has.
const BUDGET: Duration = Duration::from_millis(50);

/// T4.4 (the lifetime half): a Source whose run takes 300 ms, read once its
/// record is past its 1 s poll. The default read waits its 50 ms budget and
/// answers the stored record while `:cache` says the run is executing; a read
/// with `wait` waits for the run. A server with no scheduler, so nothing but
/// the reads runs the Source and the record ages as the clock does.
fn budget_accuracy() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let dir = TempDir::new().expect("socket dir");
    let sock = dir.path().join("s");
    let layer: toml::Value = toml::from_str(
        "[daemon.providers.slow.sources.s]\n\
         data = \"{{ exec('sleep 0.3; echo 1') | trim }}\"\n\
         result.v = \"{{ data }}\"\n\
         return = \"{{ result | poll('1s') }}\"\n",
    )
    .expect("the bench Source parses");
    let config = Config::from_layers(vec![
        beachcomber::daemon::defaults::comb_layer(),
        (PathBuf::from("<bench>"), layer),
    ]);
    {
        let _enter = rt.enter();
        let watchers = std::sync::Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
        let cache = std::sync::Arc::new(beachcomber::cache::Cache::with_watchers(watchers.clone()));
        let registry = beachcomber::provider::registry::ProviderRegistry::with_config(
            &config,
            &Default::default(),
        )
        .expect("the bench lines load")
        .share(&cache);
        beachcomber::server::Server::new(sock.clone(), cache, registry, None, watchers)
            .spawn()
            .expect("bind the bench socket");
    }
    let stream = UnixStream::connect(&sock).expect("connect");
    let mut conn = Conn {
        reader: BufReader::new(stream.try_clone().expect("clone stream")),
        writer: stream,
    };
    let get = |conn: &mut Conn, wait: bool| {
        let t0 = Instant::now();
        let answer = conn.send(&json!({"op": "get", "key": "slow.v", "wait": wait}));
        (answer, t0.elapsed())
    };
    let executing = |conn: &mut Conn| {
        conn.send(&json!({"op": "get", "key": "slow:cache.v"}))["data"]["executing"] == json!(true)
    };
    let until_idle = |conn: &mut Conn| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while executing(conn) {
            assert!(Instant::now() < deadline, "the run never ended");
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    // A record is stale once its age passes its poll, in whole seconds.
    let age_past_the_poll = || std::thread::sleep(Duration::from_millis(2100));
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;

    println!(
        "budget_accuracy: a 300ms Source, its record stale, budget {}ms, {BUDGET_ROUNDS} rounds",
        BUDGET.as_millis()
    );
    let (cold, took) = get(&mut conn, false);
    assert_eq!(cold["data"], json!("1"), "{cold}");
    println!("  cold read (waits for its run): {:.1}ms", ms(took));
    for round in 1..=BUDGET_ROUNDS {
        age_past_the_poll();
        let (late, took) = get(&mut conn, false);
        let busy = executing(&mut conn);
        println!(
            "  {round}: default read {:>6.1}ms  stale {}  executing {busy}",
            ms(took),
            late["stale"]
        );
        until_idle(&mut conn);
        age_past_the_poll();
        let (waited, took) = get(&mut conn, true);
        println!(
            "     wait read    {:>6.1}ms  stale {}",
            ms(took),
            waited["stale"]
        );
    }
}

criterion_group! {
    name = warm;
    config = Criterion::default().sample_size(200).measurement_time(Duration::from_secs(3));
    targets = bench_warm_gets
}

fn main() {
    // A filter is the first argument that is not a flag (`cargo bench` passes
    // `--bench`); with none, every part runs.
    let filter = std::env::args().skip(1).find(|a| !a.starts_with('-'));
    let runs = |name: &str| filter.as_deref().is_none_or(|f| name.contains(f));
    if runs("warm_get/") {
        warm();
        Criterion::default().configure_from_args().final_summary();
    }
    if runs("warm_get_percentiles") {
        warm_get_percentiles();
    }
    if runs("second_get_after_cold") {
        second_get_after_cold();
    }
    if runs("budget_accuracy") {
        budget_accuracy();
    }
}
