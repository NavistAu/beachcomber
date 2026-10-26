//! End-to-end tests for the cache lifecycle.
//! Unit tests for the registry itself live in src/scheduler/lifecycle.rs.
//! These exercise the full scheduler + cache + a fake provider.

use beachcomber::cache::Cache;
use beachcomber::config::Config;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    Source, SourceMetadata, SourceResult, Value,
};
use beachcomber::query::Instance;
use beachcomber::scheduler::{Scheduler, SchedulerHandle, SchedulerMessage};
use beachcomber::watcher_registry::WatcherRegistry;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use crate::common::poll::poll_until_async;

fn lc_counter_source_meta() -> SourceMetadata {
    SourceMetadata {
        name: "main".into(),
        fields: vec![FieldSchema {
            name: "value".into(),
            field_type: FieldType::Int,
        }],
        invalidation: InvalidationStrategy::Poll {
            interval_secs: 5,
            count: 2,
        },
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 30,
        },
        fsevents_reinstate: false,
        decay: None,
        budget: beachcomber::scheduler::lifecycle::DEFAULT_BUDGET,
        timeout: beachcomber::scheduler::lifecycle::DEFAULT_TIMEOUT,
        nearest: false,
    }
}

struct LcCounterSourceImpl {
    counter: Arc<AtomicU32>,
}

impl Source for LcCounterSourceImpl {
    fn metadata(&self) -> &SourceMetadata {
        use std::sync::OnceLock;
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(lc_counter_source_meta)
    }

    fn execute(&self, _path: Option<&str>) -> Result<SourceResult, String> {
        let count = self.counter.fetch_add(1, Ordering::SeqCst);
        let mut result = SourceResult::new();
        result.insert("value", Value::Int(count as i64));
        Ok(result)
    }
}

/// A simple global provider that increments a per-test counter on each execute.
struct CountingGlobalProvider {
    counter: Arc<AtomicU32>,
}

impl CountingGlobalProvider {
    fn new(counter: Arc<AtomicU32>) -> Self {
        Self { counter }
    }
}

impl Provider for CountingGlobalProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "lc_counter".to_string(),
            sources: vec![lc_counter_source_meta()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![Box::new(LcCounterSourceImpl {
            counter: Arc::clone(&self.counter),
        })]
    }
}

/// Verifies that a cold cache miss via QueryActivity immediately triggers provider execution
/// and the cache entry is populated.
#[tokio::test]
async fn integration_cold_miss_populates_cache_and_enters_active() {
    let poll_count = Arc::new(AtomicU32::new(0));

    let cache = Arc::new(Cache::new());
    let mut registry = ProviderRegistry::new();
    registry
        .register(Box::new(CountingGlobalProvider::new(Arc::clone(
            &poll_count,
        ))))
        .expect("lc_counter");
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

    // Before any query, the cache should be empty.
    assert!(
        cache.get_entry("lc_counter", None).is_none(),
        "cache should be cold before first query"
    );

    // Signal demand — this should trigger inline execution.
    handle
        .send(SchedulerMessage::QueryActivity {
            provider: "lc_counter".to_string(),
            instances: vec![Instance {
                source: "main".to_string(),
                root: None,
            }],
        })
        .await;

    // Wait on the observable the sleep was standing in for: the cache entry the
    // spawn_blocking execution writes. The counter is bumped inside execute(),
    // before that write, so it is already final once the entry appears.
    let entry = poll_until_async(
        "lc_counter cache entry",
        "Some(_)",
        Duration::from_secs(5),
        Duration::from_millis(10),
        async || cache.get_entry("lc_counter", None),
        |e| e.is_some(),
    )
    .await;

    assert!(
        entry.is_some(),
        "cache should be populated after cold miss QueryActivity"
    );
    assert!(
        poll_count.load(Ordering::SeqCst) >= 1,
        "provider should have executed at least once"
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}

/// Verifies that the scheduler status reports a poll timer for an active entry.
#[tokio::test]
async fn integration_active_entry_appears_in_status() {
    let poll_count = Arc::new(AtomicU32::new(0));

    let cache = Arc::new(Cache::new());
    let mut registry = ProviderRegistry::new();
    registry
        .register(Box::new(CountingGlobalProvider::new(Arc::clone(
            &poll_count,
        ))))
        .expect("lc_counter");
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

    handle
        .send(SchedulerMessage::QueryActivity {
            provider: "lc_counter".to_string(),
            instances: vec![Instance {
                source: "main".to_string(),
                root: None,
            }],
        })
        .await;

    // The poll-timer entry is the positive event; demand/lifecycle are filled
    // in the same build_status pass, so asserting on this same snapshot is
    // sound.
    let status = poll_until_async(
        "scheduler status poll_timers",
        "an entry for lc_counter",
        Duration::from_secs(5),
        Duration::from_millis(10),
        async || handle.get_status().await.expect("status available"),
        |s| s.poll_timers.iter().any(|t| t.provider == "lc_counter"),
    )
    .await;

    // The entry should appear in poll_timers (it's Active with a 5s poll interval).
    assert!(
        status
            .poll_timers
            .iter()
            .any(|t| t.provider == "lc_counter"),
        "lc_counter should appear in poll_timers when Active; got {:?}",
        status.poll_timers
    );

    // Active entries appear in demand, not lifecycle (decay).
    assert!(
        status.demand.iter().any(|d| d.provider == "lc_counter"),
        "lc_counter should appear in demand when Active; got {:?}",
        status.demand
    );
    assert!(
        !status.lifecycle.iter().any(|b| b.provider == "lc_counter"),
        "lc_counter should NOT be in lifecycle when Active; got {:?}",
        status.lifecycle
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}

/// Helper: start a scheduler and register the counting provider.
async fn setup_lifecycle_scheduler(
    poll_count: Arc<AtomicU32>,
) -> (Arc<Cache>, SchedulerHandle, tokio::task::JoinHandle<()>) {
    let cache = Arc::new(Cache::new());
    let mut registry = ProviderRegistry::new();
    registry
        .register(Box::new(CountingGlobalProvider::new(poll_count)))
        .expect("lc_counter");
    let registry = Arc::new(registry);
    let config = Config::default();

    let (handle, mut scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
    scheduler.use_polling_watcher(Duration::from_millis(50));
    let task = tokio::spawn(async move { scheduler.run().await });
    (cache, handle, task)
}

/// Verifies that the scheduler reports decay=0 for an Active cache entry.
#[tokio::test]
async fn integration_status_response_reports_decay_level() {
    let poll_count = Arc::new(AtomicU32::new(0));
    let (cache, handle, sched_task) = setup_lifecycle_scheduler(Arc::clone(&poll_count)).await;

    // Trigger demand so the entry enters Active state.
    handle
        .send(SchedulerMessage::QueryActivity {
            provider: "lc_counter".to_string(),
            instances: vec![Instance {
                source: "main".to_string(),
                root: None,
            }],
        })
        .await;

    // The lifecycle entry is created while QueryActivity is handled, before the
    // execution it dispatches writes the cache — so a warm cache entry is a
    // fence for the lifecycle registry having the key too.
    let entry = poll_until_async(
        "lc_counter cache entry",
        "Some(_)",
        Duration::from_secs(5),
        Duration::from_millis(10),
        async || cache.get_entry("lc_counter", None),
        |e| e.is_some(),
    )
    .await;

    assert!(entry.is_some(), "cache should be warm before status check");

    // Fetch the lifecycle snapshots from the scheduler.
    let snapshots = handle.get_lifecycle_snapshots().await;

    let key = beachcomber::scheduler::lifecycle::Key {
        provider: "lc_counter".to_string(),
        path: None,
        source: "main".to_string(),
        node: String::new(),
    };
    let decay_level = snapshots.get(&key).map(|s| s.decay);

    assert_eq!(
        decay_level,
        Some(0),
        "lc_counter should have decay=0 (Active) shortly after first query; got {:?}",
        decay_level
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}

/// Verifies that repeated QueryActivity calls keep the entry warm and don't re-execute
/// if the cache is already populated.
#[tokio::test]
async fn integration_repeated_queries_keep_data_warm() {
    let poll_count = Arc::new(AtomicU32::new(0));

    let cache = Arc::new(Cache::new());
    let mut registry = ProviderRegistry::new();
    registry
        .register(Box::new(CountingGlobalProvider::new(Arc::clone(
            &poll_count,
        ))))
        .expect("lc_counter");
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

    // First query populates cache.
    handle
        .send(SchedulerMessage::QueryActivity {
            provider: "lc_counter".to_string(),
            instances: vec![Instance {
                source: "main".to_string(),
                root: None,
            }],
        })
        .await;

    poll_until_async(
        "lc_counter cache entry",
        "Some(_)",
        Duration::from_secs(5),
        Duration::from_millis(10),
        async || cache.get_entry("lc_counter", None),
        |e| e.is_some(),
    )
    .await;

    let _entry_after_first = cache
        .get_entry("lc_counter", None)
        .expect("should have entry");
    let exec_after_first = poll_count.load(Ordering::SeqCst);

    // Subsequent queries should not re-execute (cache is warm, no inline miss).
    // A warm cache means QueryActivity dispatches no work at all, so a status
    // round-trip after each send (FIFO channel) is a complete fence: once it
    // replies, everything that query was ever going to do has happened.
    for _ in 0..5 {
        handle
            .send(SchedulerMessage::QueryActivity {
                provider: "lc_counter".to_string(),
                instances: vec![Instance {
                    source: "main".to_string(),
                    root: None,
                }],
            })
            .await;
        assert!(
            handle.get_status().await.is_some(),
            "scheduler should reply"
        );
    }

    let exec_after_repeated = poll_count.load(Ordering::SeqCst);
    let _entry_after_repeated = cache
        .get_entry("lc_counter", None)
        .expect("should still have entry");

    // Execution count should be the same (no re-execution triggered by repeat queries).
    assert_eq!(
        exec_after_first, exec_after_repeated,
        "exec count should not change for repeated queries on warm cache"
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}

/// A Source that is called is demanded when its caller is read, and only then:
/// a run of the caller (a poll of a caller that is decaying) does not keep it
/// at full speed, so the two decay together.
#[tokio::test]
async fn a_called_sources_demand_follows_its_callers_reads() {
    use beachcomber::provider::source_call::run_call;
    use beachcomber::scheduler::lifecycle::Key;

    let config = Config::from_toml(
        r#"
[daemon.providers.callee.sources.t]
data = "{{ 1 }}"
result.v = "{{ data }}"

[daemon.providers.caller.sources.s]
data = "{{ daemon.providers.callee.sources.t().v }}"
result.x = "{{ data }}"
"#,
    );
    let cache = Arc::new(Cache::new());
    let registry = ProviderRegistry::with_config(&config, &Default::default())
        .expect("loads")
        .share(&cache);
    let (handle, mut scheduler) = Scheduler::new(
        cache.clone(),
        registry.clone(),
        config,
        Arc::new(WatcherRegistry::new()),
    );
    scheduler.use_polling_watcher(Duration::from_millis(50));
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    let key = |provider: &str, source: &str| Key {
        provider: provider.into(),
        path: None,
        source: source.into(),
        node: String::new(),
    };

    // The caller runs, as a poll of it does, and calls the callee.
    // A run is blocking work: the call waits for it on a blocking thread.
    let ran = {
        let (registry, cache) = (registry.clone(), cache.clone());
        tokio::task::spawn_blocking(move || run_call(&registry, &cache, "caller", "s", None))
            .await
            .unwrap()
    }
    .expect("runs");
    assert!(ran.is_some());
    let snapshots = handle.get_lifecycle_snapshots().await;
    assert!(
        !snapshots.contains_key(&key("callee", "t")),
        "a run of the caller is not a read: it demands nothing"
    );

    // A read of the caller demands it, and what it calls with it.
    handle
        .send(SchedulerMessage::QueryActivity {
            provider: "caller".into(),
            instances: vec![Instance::new("s", None)],
        })
        .await;
    let snapshots = handle.get_lifecycle_snapshots().await;
    for (provider, source) in [("caller", "s"), ("callee", "t")] {
        let snapshot = snapshots
            .get(&key(provider, source))
            .unwrap_or_else(|| panic!("{provider}.{source} is demanded with its caller"));
        assert_eq!(snapshot.decay, 0, "{provider}.{source} starts Active");
    }

    sched_task.abort();
}
