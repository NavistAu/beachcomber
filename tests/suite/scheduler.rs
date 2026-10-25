use beachcomber::cache::Cache;
use beachcomber::provider::registry::ProviderRegistry;
use beachcomber::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    Source, SourceMetadata, SourceResult, Value,
};
use beachcomber::query::Instance;
use beachcomber::scheduler::{Scheduler, SchedulerMessage};
use beachcomber::watcher_registry::WatcherRegistry;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::common::poll::poll_until_async;

#[tokio::test]
async fn scheduler_refresh_populates_cache() {
    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let config = beachcomber::config::Config::default();

    let (handle, mut scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
    scheduler.use_polling_watcher(Duration::from_millis(50));
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    handle
        .send(SchedulerMessage::Refresh {
            provider: "hostname".to_string(),
            instances: vec![Instance {
                source: "host".to_string(),
                root: None,
            }],
        })
        .await;

    let entry = poll_until_async(
        "hostname cache entry",
        "Some(_)",
        Duration::from_secs(5),
        Duration::from_millis(10),
        async || cache.get_entry("hostname", None),
        |e| e.is_some(),
    )
    .await;
    assert!(entry.is_some(), "Refresh should populate cache");
    let _entry = entry.unwrap();
    // hostname.host source should have a 'name' field
    let name_value = cache.get_field("hostname", None, "name");
    assert!(
        name_value.is_some(),
        "hostname 'name' field should be populated"
    );
    assert!(!name_value.unwrap().0.as_text().is_empty());

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}

#[tokio::test]
async fn scheduler_refresh_unknown_provider() {
    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let config = beachcomber::config::Config::default();

    let (handle, mut scheduler) = Scheduler::new(
        cache.clone(),
        registry,
        config,
        Arc::new(WatcherRegistry::new()),
    );
    scheduler.use_polling_watcher(Duration::from_millis(50));
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    handle
        .send(SchedulerMessage::Refresh {
            provider: "nonexistent".to_string(),
            instances: vec![],
        })
        .await;

    // The channel is FIFO: a status round-trip that completes proves the
    // unknown-provider Refresh was already processed (and did not wedge the
    // loop), which is the whole point of the test.
    assert!(
        handle.get_status().await.is_some(),
        "scheduler should still serve status after an unknown-provider refresh"
    );

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}

#[tokio::test]
async fn scheduler_shutdown() {
    let cache = Arc::new(Cache::new());
    let registry = Arc::new(ProviderRegistry::with_defaults());
    let config = beachcomber::config::Config::default();

    let (handle, mut scheduler) =
        Scheduler::new(cache, registry, config, Arc::new(WatcherRegistry::new()));
    scheduler.use_polling_watcher(Duration::from_millis(50));
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    handle.send(SchedulerMessage::Shutdown).await;

    let result = tokio::time::timeout(Duration::from_secs(2), sched_task).await;
    assert!(result.is_ok(), "Scheduler should shut down within timeout");
}

// ── a Poll source registers no filesystem watch ──────────────────────────────

// One counter per source; nextest gives the test its own process.
static POLLED_RUNS: AtomicUsize = AtomicUsize::new(0);
static WATCHED_RUNS: AtomicUsize = AtomicUsize::new(0);

/// A Source that counts its executions, so "re-executed" is observable.
struct CountingSource {
    meta: SourceMetadata,
    runs: &'static AtomicUsize,
}

impl Source for CountingSource {
    fn metadata(&self) -> &SourceMetadata {
        &self.meta
    }
    fn execute(&self, _path: Option<&str>) -> Result<SourceResult, String> {
        let n = self.runs.fetch_add(1, Ordering::SeqCst) + 1;
        let mut result = SourceResult::new();
        result.insert(&self.meta.fields[0].name, Value::Int(n as i64));
        Ok(result)
    }
}

fn path_source(name: &str, invalidation: InvalidationStrategy) -> SourceMetadata {
    // Meaningless on Poll (the registry rejects it there), the default on Watch.
    let fsevents_reinstate = !matches!(invalidation, InvalidationStrategy::Poll { .. });
    SourceMetadata {
        name: name.into(),
        fields: vec![FieldSchema {
            name: format!("{name}_runs"),
            field_type: FieldType::Int,
        }],
        invalidation,
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 30,
        },
        fsevents_reinstate,
        decay: None,
        budget_execute: beachcomber::scheduler::lifecycle::DEFAULT_BUDGET_EXECUTE,
        timeout: beachcomber::scheduler::lifecycle::DEFAULT_TIMEOUT,
        nearest: false,
    }
}

/// `w` has one `Poll` source and one `Watch` source, both rooted. The
/// intervals are an hour so nothing in the test fires on a timer.
struct PollAndWatchProvider;

impl PollAndWatchProvider {
    fn polled() -> SourceMetadata {
        path_source(
            "polled",
            InvalidationStrategy::Poll {
                interval_secs: 3600,
                count: 2,
            },
        )
    }
    fn watched() -> SourceMetadata {
        path_source(
            "watched",
            InvalidationStrategy::Watch {
                patterns: vec!["foo.txt".into()],
                abs_paths: vec![],
            },
        )
    }
}

impl Provider for PollAndWatchProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "w".to_string(),
            sources: vec![Self::polled(), Self::watched()],
        }
    }
    fn sources(&self) -> Vec<Box<dyn Source>> {
        vec![
            Box::new(CountingSource {
                meta: Self::polled(),
                runs: &POLLED_RUNS,
            }),
            Box::new(CountingSource {
                meta: Self::watched(),
                runs: &WATCHED_RUNS,
            }),
        ]
    }
}

async fn wait_for_runs(what: &str, counter: &'static AtomicUsize, n: usize) {
    poll_until_async(
        what,
        &n.to_string(),
        Duration::from_secs(5),
        Duration::from_millis(10),
        async || counter.load(Ordering::SeqCst),
        |runs| *runs == n,
    )
    .await;
}

/// Canon provider_source.md invariant 4: a `Poll` Source never registers a
/// filesystem watch. It used to — with an empty pattern list, which matches
/// every event under the root — so a write anywhere in the tree re-executed
/// it. A write the `Watch` sibling's pattern does not name re-executes
/// neither; one it does name re-executes only the `Watch` source.
#[tokio::test]
async fn a_poll_source_registers_no_watch_and_a_watch_source_matches_only_its_patterns() {
    let root_dir = tempfile::TempDir::new().unwrap();
    let root = root_dir.path().canonicalize().unwrap();

    let cache = Arc::new(Cache::new());
    let mut registry = ProviderRegistry::new();
    registry.register(Box::new(PollAndWatchProvider)).unwrap();
    let (handle, mut scheduler) = Scheduler::new(
        cache,
        Arc::new(registry),
        beachcomber::config::Config::default(),
        Arc::new(WatcherRegistry::new()),
    );
    scheduler.use_polling_watcher(Duration::from_millis(50));
    let sched_task = tokio::spawn(async move { scheduler.run().await });

    // Demand at `root`: both sources go Cold -> Active and run once, and the
    // Watch source's subscription on `root` is registered before either runs.
    handle
        .send(SchedulerMessage::QueryActivity {
            provider: "w".to_string(),
            instances: vec![
                Instance {
                    source: "polled".to_string(),
                    root: Some(root.to_string_lossy().into_owned()),
                },
                Instance {
                    source: "watched".to_string(),
                    root: Some(root.to_string_lossy().into_owned()),
                },
            ],
        })
        .await;
    wait_for_runs("polled source first run", &POLLED_RUNS, 1).await;
    wait_for_runs("watched source first run", &WATCHED_RUNS, 1).await;

    // A write no pattern names: nothing re-executes. Proven below by ordering
    // — this event is delivered before (or with) the one that follows, and
    // the scheduler loop handles them in order.
    std::fs::write(root.join("foo.md"), "1").unwrap();

    // A write the Watch pattern names: only the Watch source re-executes.
    std::fs::write(root.join("foo.txt"), "1").unwrap();
    wait_for_runs("watched source after foo.txt", &WATCHED_RUNS, 2).await;

    // One more named write, so any execution the foo.md event could have
    // started has finished by the time the counts below are read.
    std::fs::write(root.join("foo.txt"), "2").unwrap();
    wait_for_runs("watched source after the second foo.txt", &WATCHED_RUNS, 3).await;

    assert_eq!(
        POLLED_RUNS.load(Ordering::SeqCst),
        1,
        "the Poll source ran once, on demand, and never on an fs event"
    );
    assert_eq!(WATCHED_RUNS.load(Ordering::SeqCst), 3);

    handle.send(SchedulerMessage::Shutdown).await;
    let _ = sched_task.await;
}
