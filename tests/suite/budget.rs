//! A read waits up to its budget for a stale record's run (phase 4 §2.5).
//!
//! The Sources here are gated: each run takes a permit the test grants, so
//! "answered within the budget" is "answered while the run was still held",
//! which is deterministic, rather than a time.

use beachcomber::cache::Cache;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    Source, SourceMetadata, SourceResult, Value,
};
use beachcomber::server::Server;
use beachcomber::watcher_registry::WatcherRegistry;
use serde_json::{Value as Json, json};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Runs a Source may make: each run takes one, waiting until the test grants
/// it.
#[derive(Default)]
struct Permits {
    left: Mutex<u32>,
    granted: Condvar,
}

impl Permits {
    fn grant(&self, n: u32) {
        *self.left.lock().unwrap() += n;
        self.granted.notify_all();
    }

    fn take(&self) {
        let mut left = self.left.lock().unwrap();
        while *left == 0 {
            left = self.granted.wait(left).unwrap();
        }
        *left -= 1;
    }
}

/// A Source that answers its run count once it has a permit.
struct Gated {
    meta: SourceMetadata,
    runs: Arc<AtomicU32>,
    permits: Arc<Permits>,
}

impl Source for Gated {
    fn metadata(&self) -> &SourceMetadata {
        &self.meta
    }

    fn execute(&self, _: Option<&str>) -> Result<SourceResult, String> {
        let run = self.runs.fetch_add(1, Ordering::SeqCst) + 1;
        self.permits.take();
        let mut result = SourceResult::new();
        result.insert(self.meta.fields[0].name.clone(), Value::Int(i64::from(run)));
        Ok(result)
    }
}

/// One gated Source of the provider `g`.
struct Side {
    runs: Arc<AtomicU32>,
    permits: Arc<Permits>,
}

fn meta(source: &str, field: &str, budget: Duration, failback: u32) -> SourceMetadata {
    SourceMetadata {
        name: source.into(),
        fields: vec![FieldSchema {
            name: field.into(),
            field_type: FieldType::Int,
        }],
        invalidation: InvalidationStrategy::Poll {
            interval_secs: 60,
            count: 2,
        },
        failback: FailbackConfig {
            reattempts: failback,
            interval_secs: 3600,
        },
        decay: None,
        budget,
        timeout: Duration::from_secs(600),
        nearest: false,
    }
}

struct GProvider(Vec<(SourceMetadata, Arc<AtomicU32>, Arc<Permits>)>);

impl Provider for GProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "g".into(),
            sources: self.0.iter().map(|(m, _, _)| m.clone()).collect(),
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        self.0
            .iter()
            .map(|(meta, runs, permits)| {
                Box::new(Gated {
                    meta: meta.clone(),
                    runs: runs.clone(),
                    permits: permits.clone(),
                }) as Box<dyn Source>
            })
            .collect()
    }
}

/// A server of `g`, whose Sources are `sources`; the permits are granted in
/// plenty when it is dropped, so a failing test leaves no run held.
struct Daemon {
    sock: std::path::PathBuf,
    _dir: tempfile::TempDir,
    sides: Vec<Side>,
    handle: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        for side in &self.sides {
            side.permits.grant(1000);
        }
        self.handle.abort();
    }
}

fn daemon(sources: Vec<SourceMetadata>) -> Daemon {
    let sides: Vec<Side> = sources
        .iter()
        .map(|_| Side {
            runs: Arc::default(),
            permits: Arc::default(),
        })
        .collect();
    let mut registry = ProviderRegistry::new();
    registry
        .register(Box::new(GProvider(
            sources
                .into_iter()
                .zip(&sides)
                .map(|(m, s)| (m, s.runs.clone(), s.permits.clone()))
                .collect(),
        )))
        .unwrap();
    let watchers = Arc::new(WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("s");
    let handle = Server::new(sock.clone(), cache, Arc::new(registry), None, watchers)
        .spawn()
        .unwrap();
    Daemon {
        sock,
        _dir: dir,
        sides,
        handle,
    }
}

async fn ask(sock: std::path::PathBuf, request: Json) -> Json {
    let mut stream = UnixStream::connect(&sock).await.unwrap();
    let mut line = request.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes()).await.unwrap();
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer).await.unwrap();
    serde_json::from_str(&answer).expect("a JSON answer")
}

fn get(key: &str) -> Json {
    json!({"op": "get", "key": key})
}

/// Age every record past its 60 s poll: the cache stamps records on tokio's
/// clock, so the offset survives `resume`.
async fn age_past_the_poll() {
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::time::resume();
}

/// Yield until `done`: a run's thread moves in real time, which the runtime
/// does not see.
async fn until(what: &str, done: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "never: {what}");
        tokio::task::yield_now().await;
    }
}

/// A short budget, so a read that waits past it shows quickly.
const BUDGET: Duration = Duration::from_millis(10);

/// The first read runs the Source and stores 1; the record ages past its poll.
async fn stale(d: &Daemon, key: &str) {
    d.sides[0].permits.grant(1);
    assert_eq!(ask(d.sock.clone(), get(key)).await["data"], json!(1));
    age_past_the_poll().await;
}

/// A read of a record past its lifetime runs it, and waits up to its Source's
/// budget: while the run is held the read answers the record it has, stale,
/// and `:cache` says a run is executing. Once the run is let through, the
/// next read has its value, fresh.
#[tokio::test]
async fn a_stale_record_answers_within_its_budget_and_the_run_goes_on() {
    let d = daemon(vec![meta("s", "n", BUDGET, 3)]);
    stale(&d, "g.n").await;

    let late = ask(d.sock.clone(), get("g.n")).await;
    assert_eq!(d.sides[0].runs.load(Ordering::SeqCst), 2, "the read ran it");
    assert_eq!(
        late["data"],
        json!(1),
        "the stored value, while the run is held"
    );
    assert_eq!(late["stale"], json!(true));
    let cache = ask(d.sock.clone(), get("g:cache.n")).await;
    assert_eq!(cache["data"]["executing"], json!(true), "{cache}");

    d.sides[0].permits.grant(1);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let answer = ask(d.sock.clone(), get("g.n")).await;
        if answer["data"] == json!(2) {
            assert_eq!(answer["stale"], json!(false));
            break;
        }
        assert!(std::time::Instant::now() < deadline, "the run never stored");
        tokio::task::yield_now().await;
    }
    let cache = ask(d.sock.clone(), get("g:cache.n")).await;
    assert_eq!(cache["data"]["executing"], json!(false), "{cache}");
    assert_eq!(
        d.sides[0].runs.load(Ordering::SeqCst),
        2,
        "one run, not one per read"
    );
}

/// Whether a read is still waiting after `for`: the direction a loaded
/// machine cannot flake, since it only ever makes the read slower.
async fn still_waiting(read: &mut tokio::task::JoinHandle<Json>, r#for: Duration) -> bool {
    tokio::time::timeout(r#for, &mut *read).await.is_err()
}

/// A Source whose budget is an hour: a read of its stale record waits for the
/// run, and answers its value.
#[tokio::test]
async fn a_budget_of_an_hour_waits_for_the_run() {
    let d = daemon(vec![meta("s", "n", Duration::from_secs(3600), 3)]);
    stale(&d, "g.n").await;
    let mut read = tokio::spawn(ask(d.sock.clone(), get("g.n")));
    until("the run to begin", || {
        d.sides[0].runs.load(Ordering::SeqCst) == 2
    })
    .await;
    assert!(still_waiting(&mut read, BUDGET * 5).await);
    d.sides[0].permits.grant(1);
    let answer = read.await.unwrap();
    assert_eq!(answer["data"], json!(2));
    assert_eq!(answer["stale"], json!(false));
}

/// A read with nothing stored waits for its run, past the budget: there is
/// nothing to answer with until it ends.
#[tokio::test]
async fn a_cold_miss_waits_for_its_run() {
    let d = daemon(vec![meta("s", "n", BUDGET, 3)]);
    let mut read = tokio::spawn(ask(d.sock.clone(), get("g.n")));
    until("the run to begin", || {
        d.sides[0].runs.load(Ordering::SeqCst) == 1
    })
    .await;
    assert!(still_waiting(&mut read, BUDGET * 5).await);
    d.sides[0].permits.grant(1);
    assert_eq!(read.await.unwrap()["data"], json!(1));
}

/// `--force` waits for the run it makes, past the budget, and answers it.
#[tokio::test]
async fn force_waits_for_its_run() {
    let d = daemon(vec![meta("s", "n", BUDGET, 3)]);
    d.sides[0].permits.grant(1);
    assert_eq!(ask(d.sock.clone(), get("g.n")).await["data"], json!(1));
    let mut read = tokio::spawn(ask(
        d.sock.clone(),
        json!({"op": "get", "key": "g.n", "force": true}),
    ));
    until("the run to begin", || {
        d.sides[0].runs.load(Ordering::SeqCst) == 2
    })
    .await;
    assert!(still_waiting(&mut read, BUDGET * 5).await);
    d.sides[0].permits.grant(1);
    assert_eq!(read.await.unwrap()["data"], json!(2));
}

/// A read of the whole namespace starts the run of each Source it needs
/// before it waits for any: both gated runs are under way together.
#[tokio::test]
async fn a_namespace_read_starts_every_run_before_it_waits() {
    let d = daemon(vec![meta("a", "x", BUDGET, 3), meta("b", "y", BUDGET, 3)]);
    let read = tokio::spawn(ask(d.sock.clone(), get("g")));
    until("both runs to begin, neither let through", || {
        d.sides.iter().all(|s| s.runs.load(Ordering::SeqCst) == 1)
    })
    .await;
    for side in &d.sides {
        side.permits.grant(1);
    }
    assert_eq!(read.await.unwrap()["data"], json!({"x": 1, "y": 1}));
}

/// An instance in failback backoff runs nothing for a read, stale or not: the
/// read answers what is stored, at once.
#[tokio::test]
async fn an_instance_in_failback_backoff_is_read_as_it_is() {
    struct Failing(SourceMetadata, Arc<AtomicU32>);
    impl Source for Failing {
        fn metadata(&self) -> &SourceMetadata {
            &self.0
        }
        fn execute(&self, _: Option<&str>) -> Result<SourceResult, String> {
            let run = self.1.fetch_add(1, Ordering::SeqCst);
            if run == 0 {
                let mut result = SourceResult::new();
                result.insert("n", Value::Int(1));
                return Ok(result);
            }
            Err("down".into())
        }
    }
    struct P(SourceMetadata, Arc<AtomicU32>);
    impl Provider for P {
        fn metadata(&self) -> ProviderMetadata {
            ProviderMetadata {
                name: "g".into(),
                sources: vec![self.0.clone()],
            }
        }
        fn sources(&self) -> Vec<Box<dyn Source>> {
            vec![Box::new(Failing(self.0.clone(), self.1.clone()))]
        }
    }
    let runs = Arc::new(AtomicU32::new(0));
    let mut registry = ProviderRegistry::new();
    registry
        .register(Box::new(P(meta("s", "n", BUDGET, 1), runs.clone())))
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("s");
    let watchers = Arc::new(WatcherRegistry::new());
    let handle = Server::new(
        sock.clone(),
        Arc::new(Cache::with_watchers(watchers.clone())),
        Arc::new(registry),
        None,
        watchers,
    )
    .spawn()
    .unwrap();
    assert_eq!(ask(sock.clone(), get("g.n")).await["data"], json!(1));
    age_past_the_poll().await;
    // The stale read runs it; the run fails and the instance backs off.
    let answer = ask(sock.clone(), get("g.n")).await;
    assert_eq!(answer["data"], json!(1));
    until("the failing run", || runs.load(Ordering::SeqCst) == 2).await;
    let answer = ask(sock.clone(), get("g.n")).await;
    assert_eq!(answer["data"], json!(1), "as it is");
    assert_eq!(answer["stale"], json!(true));
    assert_eq!(runs.load(Ordering::SeqCst), 2, "no run in backoff");
    handle.abort();
}

/// A watch of the key has the late value as a frame once the run that a read
/// stopped waiting for has stored it.
#[tokio::test]
async fn a_watch_gets_the_late_value() {
    let d = daemon(vec![meta("s", "n", BUDGET, 3)]);
    stale(&d, "g.n").await;
    let stream = UnixStream::connect(&d.sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    writer
        .write_all(format!("{}\n", json!({"op": "watch", "key": "g.n"})).as_bytes())
        .await
        .unwrap();
    let mut lines = BufReader::new(reader).lines();
    let first: Json = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(first["data"], json!(1));

    assert_eq!(
        ask(d.sock.clone(), get("g.n")).await["data"],
        json!(1),
        "late"
    );
    d.sides[0].permits.grant(1);
    let frame = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .expect("a frame")
        .unwrap()
        .unwrap();
    let frame: Json = serde_json::from_str(&frame).unwrap();
    assert_eq!(frame["data"], json!(2), "the late value");
}

/// A get that says how long it waits.
fn waiting(key: &str, wait: &str) -> Json {
    json!({"op": "get", "key": key, "wait": wait})
}

/// `--wait 0` on a stale record answers it at once, though the Source's
/// budget would wait an hour: the run it started is still held, and goes on.
#[tokio::test]
async fn wait_0_answers_a_stale_record_with_its_run_held() {
    let d = daemon(vec![meta("s", "n", Duration::from_secs(3600), 3)]);
    stale(&d, "g.n").await;

    let answer = ask(d.sock.clone(), waiting("g.n", "0")).await;
    assert_eq!(answer["data"], json!(1), "the stored value: {answer}");
    assert_eq!(answer["stale"], json!(true));
    let cache = ask(d.sock.clone(), get("g:cache.n")).await;
    assert_eq!(cache["data"]["executing"], json!(true), "{cache}");
    until("the read's run to begin", || {
        d.sides[0].runs.load(Ordering::SeqCst) == 2
    })
    .await;

    d.sides[0].permits.grant(1);
    let answer = ask(d.sock.clone(), waiting("g.n", "forever")).await;
    assert_eq!(
        answer["data"],
        json!(2),
        "the run went on and stored: {answer}"
    );
}

/// `--wait 0` with nothing stored answers a miss at once, and the run it
/// started goes on in the background and stores its value.
#[tokio::test]
async fn wait_0_on_a_cold_read_is_a_miss_and_the_run_goes_on() {
    let d = daemon(vec![meta("s", "n", BUDGET, 3)]);

    let answer = ask(d.sock.clone(), waiting("g.n", "0")).await;
    assert_eq!(answer, json!({"ok": true}), "a miss, with the run held");
    until("the read's run to begin", || {
        d.sides[0].runs.load(Ordering::SeqCst) == 1
    })
    .await;

    d.sides[0].permits.grant(1);
    let answer = ask(d.sock.clone(), waiting("g.n", "forever")).await;
    assert_eq!(answer["data"], json!(1), "{answer}");
    assert_eq!(
        d.sides[0].runs.load(Ordering::SeqCst),
        1,
        "the read took the run the first one started"
    );
}

/// `--wait forever` waits for a stale record's run past its Source's budget,
/// and answers the run's value.
#[tokio::test]
async fn wait_forever_waits_past_the_budget() {
    let d = daemon(vec![meta("s", "n", BUDGET, 3)]);
    stale(&d, "g.n").await;
    let mut read = tokio::spawn(ask(d.sock.clone(), waiting("g.n", "forever")));
    until("the run to begin", || {
        d.sides[0].runs.load(Ordering::SeqCst) == 2
    })
    .await;
    assert!(still_waiting(&mut read, BUDGET * 5).await);
    d.sides[0].permits.grant(1);
    let answer = read.await.unwrap();
    assert_eq!(answer["data"], json!(2));
    assert_eq!(answer["stale"], json!(false));
}

/// `--force` with `--wait 0` starts its run and answers what is stored, the
/// last good value, while the run goes on.
#[tokio::test]
async fn force_with_wait_0_answers_the_stored_value() {
    let d = daemon(vec![meta("s", "n", BUDGET, 3)]);
    d.sides[0].permits.grant(1);
    assert_eq!(ask(d.sock.clone(), get("g.n")).await["data"], json!(1));

    let answer = ask(
        d.sock.clone(),
        json!({"op": "get", "key": "g.n", "force": true, "wait": "0"}),
    )
    .await;
    assert_eq!(answer["data"], json!(1), "the stored value: {answer}");
    until("the forced run to begin", || {
        d.sides[0].runs.load(Ordering::SeqCst) == 2
    })
    .await;

    d.sides[0].permits.grant(1);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while ask(d.sock.clone(), get("g.n")).await["data"] != json!(2) {
        assert!(std::time::Instant::now() < deadline, "the run never stored");
        tokio::task::yield_now().await;
    }
}

/// The boolean `wait` it replaced is an invalid request.
#[tokio::test]
async fn a_boolean_wait_is_refused() {
    let d = daemon(vec![meta("s", "n", BUDGET, 3)]);
    let answer = ask(
        d.sock.clone(),
        json!({"op": "get", "key": "g.n", "wait": true}),
    )
    .await;
    assert_eq!(answer["ok"], json!(false), "{answer}");
    assert!(
        answer["error"]
            .as_str()
            .is_some_and(|e| e.contains("invalid type: boolean `true`")),
        "{answer}"
    );
    assert_eq!(d.sides[0].runs.load(Ordering::SeqCst), 0, "nothing ran");
}
