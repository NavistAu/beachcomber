use beachcomber::cache::Cache;
use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{
    FailbackConfig, FieldSchema, Schedule, Source, SourceMetadata, SourceResult,
};
use beachcomber::query::Instance;
use beachcomber::scheduler::{Scheduler, SchedulerMessage};
use beachcomber::watcher_registry::WatcherRegistry;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use crate::common::poll::poll_until_async;

fn failing_source_meta() -> SourceMetadata {
    SourceMetadata {
        name: "main".into(),
        fields: vec![FieldSchema {
            name: "value".into(),
        }],
        schedule: Schedule {
            failback: FailbackConfig {
                reattempts: 3,
                interval_secs: 30,
            },
            ..Schedule::poll(60, 2)
        },
    }
}

struct FailingSourceImpl {
    counter: Arc<AtomicU32>,
}

impl Source for FailingSourceImpl {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(failing_source_meta)
    }

    fn execute(&self, _path: Option<&str>) -> Result<SourceResult, String> {
        self.counter.fetch_add(1, Ordering::SeqCst);
        Err("always fails".to_string())
    }
}

struct FailingProvider {
    counter: Arc<AtomicU32>,
}

impl FailingProvider {
    fn new(counter: Arc<AtomicU32>) -> Self {
        Self { counter }
    }
}

impl FailingProvider {
    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(FailingSourceImpl {
            counter: Arc::clone(&self.counter),
        })]
    }
}

#[tokio::test]
async fn repeated_failures_trigger_backoff() {
    let fail_exec_count = Arc::new(AtomicU32::new(0));

    let cache = Arc::new(Cache::new());
    let mut registry = ProviderRegistry::new();
    registry
        .register_sources(
            "failing",
            FailingProvider::new(Arc::clone(&fail_exec_count)).sources(),
        )
        .expect("failing");
    let registry = Arc::new(registry);
    let config = Config::default();

    let (handle, mut scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
    scheduler.use_polling_watcher(Duration::from_millis(50));
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    // Refresh 10 times, each one let to run to completion before the next.
    // That serialisation is what the old inter-refresh sleep bought, and it is
    // what makes this a backoff test rather than a dedup test: a refresh that
    // lands while the previous one is still in flight is deduplicated instead
    // of suppressed. `in_flight` empties only after the failure counter has
    // been recorded, so it is the exact fence.
    //
    // The first status round-trip after each send is also the ordering fence:
    // the channel is FIFO, so it cannot observe an empty `in_flight` from
    // before the refresh was processed.
    for _ in 0..10 {
        handle
            .send(SchedulerMessage::Refresh {
                provider: "failing".to_string(),
                instances: vec![Instance {
                    source: "main".to_string(),
                    root: beachcomber::query::Dir::Global,
                }],
            })
            .await;
        poll_until_async(
            "scheduler in_flight",
            "empty",
            Duration::from_secs(5),
            Duration::from_millis(5),
            async || {
                handle
                    .get_status()
                    .await
                    .expect("status available")
                    .in_flight
            },
            |f| f.is_empty(),
        )
        .await;
    }

    let count = fail_exec_count.load(Ordering::SeqCst);
    // After 3 consecutive failures, subsequent refreshes should be suppressed
    // So we expect fewer than 10 executions
    assert!(
        count < 10,
        "Expected failure backoff to suppress some executions, got {count}"
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}

/// A non-2xx response fails the `url()` call, so the Source's execute fails
/// and the failback applies, even when the body would have produced the
/// field: ten refreshes reach the server exactly `failback`'s count of times
/// and nothing is cached.
#[tokio::test(flavor = "multi_thread")]
async fn a_non_2xx_url_fails_the_source_and_backs_off() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/summary.json")
        .with_status(503)
        .with_body(r#"{"status":{"indicator":"minor"}}"#)
        .expect(3)
        .create_async()
        .await;
    let config = Config::from_toml(&format!(
        r#"
[daemon.providers.claude.sources.summary]
data = "{{{{ url('{}/summary.json') | fromjson }}}}"
result.indicator = "{{{{ data.status.indicator }}}}"
return = "{{{{ result | poll('60s') | failback('60s', 3) }}}}"
"#,
        server.url()
    ));

    let cache = Arc::new(Cache::new());
    let registry = Arc::new(
        ProviderRegistry::with_config(&config, &std::env::vars().collect()).expect("registry"),
    );
    let (handle, mut scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
    scheduler.use_polling_watcher(Duration::from_millis(50));
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    // Each refresh runs to completion before the next, as in the test above.
    for _ in 0..10 {
        handle
            .send(SchedulerMessage::Refresh {
                provider: "claude".to_string(),
                instances: vec![Instance {
                    source: "summary".to_string(),
                    root: beachcomber::query::Dir::Global,
                }],
            })
            .await;
        poll_until_async(
            "scheduler in_flight",
            "empty",
            Duration::from_secs(5),
            Duration::from_millis(5),
            async || {
                handle
                    .get_status()
                    .await
                    .expect("status available")
                    .in_flight
            },
            |f| f.is_empty(),
        )
        .await;
    }

    mock.assert_async().await;
    assert!(
        cache.get_entry("claude", None).is_none(),
        "a failed fetch caches nothing"
    );
    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}

/// The failback holds back the runs reads start, as it holds back the
/// scheduler's: after the failure that puts the instance into backoff a read
/// runs nothing and answers what is stored (nothing); `--force` runs it.
#[tokio::test]
async fn failback_skips_the_runs_reads_start_and_force_does_not() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    struct Once(Arc<AtomicU32>);
    impl Source for Once {
        fn metadata(&self) -> &SourceMetadata {
            use std::sync::OnceLock;
            static M: OnceLock<SourceMetadata> = OnceLock::new();
            M.get_or_init(|| SourceMetadata {
                schedule: Schedule {
                    failback: FailbackConfig {
                        reattempts: 1,
                        interval_secs: 3600,
                    },
                    ..failing_source_meta().schedule
                },
                ..failing_source_meta()
            })
        }
        fn execute(&self, _: Option<&str>) -> Result<SourceResult, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err("always fails".into())
        }
    }
    struct OnceProvider(Arc<AtomicU32>);
    impl OnceProvider {
        fn sources(&self) -> Vec<Box<dyn Source>> {
            vec![Box::new(Once(self.0.clone()))]
        }
    }

    let runs = Arc::new(AtomicU32::new(0));
    let mut registry = ProviderRegistry::new();
    registry
        .register_sources("failing", OnceProvider(runs.clone()).sources())
        .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("s");
    let watchers = Arc::new(WatcherRegistry::new());
    let server = crate::common::scheduler::server(
        sock.clone(),
        Arc::new(Cache::with_watchers(watchers.clone())),
        Arc::new(registry),
        watchers,
    );
    let handle = server.spawn().unwrap();
    let get = |force: bool| {
        let sock = sock.clone();
        async move {
            let mut stream = tokio::net::UnixStream::connect(&sock).await.unwrap();
            let request = serde_json::json!({"op": "get", "key": "failing.value", "force": force});
            stream
                .write_all(format!("{request}\n").as_bytes())
                .await
                .unwrap();
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).await.unwrap();
            serde_json::from_str::<serde_json::Value>(&line).unwrap()
        }
    };
    assert_eq!(get(false).await["data"], serde_json::Value::Null);
    assert_eq!(runs.load(Ordering::SeqCst), 1, "the cold read ran it");
    assert_eq!(get(false).await["data"], serde_json::Value::Null);
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "in backoff, a read runs nothing"
    );
    get(true).await;
    assert_eq!(runs.load(Ordering::SeqCst), 2, "--force runs it");
    handle.abort();
}

/// A Source whose `data` line reads `mode` and fails as it says (`fatal`,
/// `error`) or returns it, counting its runs in `count`, in-process with a
/// real server and scheduler.
struct Mode {
    dir: std::path::PathBuf,
    sock: std::path::PathBuf,
    cache: Arc<Cache>,
    registry: Arc<ProviderRegistry>,
    in_force: beachcomber::provider::registry::RegistryHandle,
    scheduler: beachcomber::scheduler::SchedulerHandle,
}

impl Mode {
    fn config(dir: &std::path::Path, failback: &str) -> Config {
        Config::from_toml(&format!(
            r#"
[daemon.providers.p.sources.s]
data = "{{% set n = exec('echo >> {d}/count') %}}{{{{ log.fatal('boom') if ('{d}/mode' | read | trim) == 'fatal' else (fail('soft') if ('{d}/mode' | read | trim) == 'error' else ('{d}/mode' | read | trim)) }}}}"
result.v = "{{{{ data }}}}"
return = "{{{{ result | poll('1h') | failback('{failback}', 1) }}}}"
"#,
            d = dir.display()
        ))
    }

    fn new(dir: &std::path::Path, failback: &str) -> Self {
        use beachcomber::provider::registry::RegistryHandle;
        std::fs::write(dir.join("mode"), "ok").unwrap();
        let config = Self::config(dir, failback);
        let watchers = Arc::new(WatcherRegistry::new());
        let cache = Arc::new(Cache::with_watchers(watchers.clone()));
        let registry = ProviderRegistry::with_config(&config, &std::env::vars().collect())
            .unwrap()
            .share(&cache);
        let in_force = RegistryHandle::new(registry.clone());
        let (scheduler, mut task) =
            Scheduler::new(cache.clone(), registry.clone(), config, watchers.clone());
        task.use_polling_watcher(Duration::from_millis(50));
        tokio::spawn(task.run());
        let sock = dir.join("s");
        beachcomber::server::Server::new(
            sock.clone(),
            cache.clone(),
            in_force.clone(),
            scheduler.clone(),
            watchers,
        )
        .spawn()
        .unwrap();
        Self {
            dir: dir.into(),
            sock,
            cache,
            registry,
            in_force,
            scheduler,
        }
    }

    fn set(&self, mode: &str) {
        std::fs::write(self.dir.join("mode"), mode).unwrap();
    }

    fn runs(&self) -> usize {
        std::fs::read_to_string(self.dir.join("count")).map_or(0, |t| t.lines().count())
    }

    async fn ask(&self, request: serde_json::Value) -> serde_json::Value {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let mut stream = tokio::net::UnixStream::connect(&self.sock).await.unwrap();
        stream
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }

    /// The value `p.v` reads, with `force` or not.
    async fn get(&self, force: bool) -> serde_json::Value {
        self.ask(serde_json::json!({"op": "get", "key": "p.v", "force": force}))
            .await["data"]
            .clone()
    }

    /// Wait for every run in flight to settle.
    async fn settled(&self) {
        poll_until_async(
            "scheduler in_flight",
            "empty",
            Duration::from_secs(5),
            Duration::from_millis(5),
            async || self.scheduler.get_status().await.unwrap().in_flight,
            |f| f.is_empty(),
        )
        .await;
    }

    /// What a poll (`Any`), a file event or refresh (`Fresh`), and a read
    /// that found an input changed (`Changed`) each get of the instance.
    fn starts(&self) -> [bool; 3] {
        use beachcomber::run::Want;
        let key = beachcomber::provider::source_call::instance_key("p", "s", None);
        [Want::Any, Want::Fresh, Want::Changed].map(|w| {
            self.registry
                .runs()
                .start(w, &self.registry, &self.cache, &key)
                .is_some()
        })
    }

    /// The instance as `comb status` shows its failure.
    async fn failure(&self) -> Option<beachcomber::cache::FailureSnapshot> {
        self.scheduler
            .get_failure_states()
            .await
            .into_values()
            .next()
    }

    /// Run it into a `log.fatal` after one good run: the last good value is `ok`.
    async fn stopped(dir: &std::path::Path) -> Self {
        let rig = Self::new(dir, "1h");
        assert_eq!(rig.get(false).await, "ok");
        rig.set("fatal");
        assert_eq!(rig.get(false).await, "ok", "the last good value stands");
        rig.settled().await;
        assert_eq!(rig.runs(), 2);
        rig
    }
}

/// A `log.fatal` stops the instance with no expiry: nothing starts a run of
/// it, a poll's, a watch's or a read's, and a read keeps the last good value.
/// `comb status` shows it stopped, with the message.
#[tokio::test(flavor = "multi_thread")]
async fn log_fatal_stops_the_instance_and_keeps_the_last_good_value() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = Mode::stopped(&tmp.path().canonicalize().unwrap()).await;
    let failure = rig.failure().await.expect("a failure is recorded");
    assert!(failure.stopped, "{failure:?}");
    assert!(failure.suppressed_until_unix_ms.is_none(), "no expiry");
    assert!(failure.last_error.unwrap().contains("boom"));
    assert_eq!(rig.starts(), [false; 3], "no poll, event or read runs it");
    assert_eq!(rig.get(false).await, "ok");
    assert_eq!(rig.runs(), 2);
}

/// A change to a file the failed run read re-arms it: the next read runs it.
#[tokio::test(flavor = "multi_thread")]
async fn a_changed_input_re_arms_a_stopped_instance() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = Mode::stopped(&tmp.path().canonicalize().unwrap()).await;
    rig.set("fixed value");
    // The read starts the re-armed run and answers within the Source's
    // budget: under load that is still the stored value. Wait for the run.
    rig.get(false).await;
    rig.settled().await;
    assert_eq!(rig.runs(), 3);
    assert_eq!(rig.get(false).await, "fixed value");
    assert!(rig.failure().await.is_none(), "the success ended the stop");
}

/// A refresh of a key it serves, and `get --force`, re-arm it: each runs it.
#[tokio::test(flavor = "multi_thread")]
async fn refresh_and_force_re_arm_a_stopped_instance() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = Mode::stopped(&tmp.path().canonicalize().unwrap()).await;

    rig.ask(serde_json::json!({"op": "refresh", "key": "p.v"}))
        .await;
    // The scheduler starts the run: wait for it to have run, then to settle.
    poll_until_async(
        "runs after a refresh",
        "3",
        Duration::from_secs(5),
        Duration::from_millis(5),
        async || rig.runs(),
        |n| *n == 3,
    )
    .await;
    rig.settled().await;

    rig.get(true).await;
    rig.settled().await;
    assert_eq!(rig.runs(), 4, "--force runs it");
    assert!(
        rig.failure().await.unwrap().stopped,
        "and it fails fatally again"
    );
}

/// A config reload re-arms it.
#[tokio::test(flavor = "multi_thread")]
async fn a_config_reload_re_arms_a_stopped_instance() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    let rig = Mode::stopped(&dir).await;
    let after =
        ProviderRegistry::with_config(&Mode::config(&dir, "1h"), &std::env::vars().collect())
            .unwrap()
            .adopt(&rig.registry, &rig.cache);
    rig.scheduler
        .send(SchedulerMessage::Reload {
            next: after.clone(),
            registry: rig.in_force.clone(),
        })
        .await;
    rig.scheduler
        .get_status()
        .await
        .expect("the reload was handled");
    assert!(rig.failure().await.is_none(), "the reload re-armed it");
    let key = beachcomber::provider::source_call::instance_key("p", "s", None);
    assert!(
        after
            .runs()
            .start(beachcomber::run::Want::Any, &after, &rig.cache, &key)
            .is_some()
    );
}

/// An `error` is not fatal: it backs off, and retries once the backoff ends.
#[tokio::test(flavor = "multi_thread")]
async fn an_error_still_backs_off_and_retries() {
    let tmp = tempfile::tempdir().unwrap();
    let rig = Mode::new(&tmp.path().canonicalize().unwrap(), "1s");
    assert_eq!(rig.get(false).await, "ok");
    rig.set("error");
    assert_eq!(rig.get(false).await, "ok");
    rig.settled().await;
    assert_eq!(rig.runs(), 2);
    assert!(!rig.failure().await.unwrap().stopped);
    rig.set("fixed again");
    poll_until_async(
        "the retry after the backoff",
        "the fixed value",
        Duration::from_secs(5),
        Duration::from_millis(50),
        async || rig.get(false).await,
        |v| v == "fixed again",
    )
    .await;
}
