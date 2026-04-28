use beachcomber::cache::Cache;
use beachcomber::provider::{InvalidationStrategy, Value, expected_interval_secs};
use std::collections::HashMap;

#[test]
fn expected_interval_secs_maps_all_strategies() {
    assert_eq!(
        expected_interval_secs(&InvalidationStrategy::Poll { interval_secs: 30 }),
        Some(30)
    );
    assert_eq!(
        expected_interval_secs(&InvalidationStrategy::WatchAndPoll {
            patterns: vec![".git".into()],
            abs_paths: vec![],
            interval_secs: 60,
        }),
        Some(60)
    );
    assert_eq!(
        expected_interval_secs(&InvalidationStrategy::Watch {
            patterns: vec![".git".into()],
            abs_paths: vec![],
        }),
        None
    );
}

/// Uses tokio mock clock: `start_paused = true` + `advance` avoids a real 1.1 s wall-clock wait.
/// `is_stale()` calls `tokio::time::Instant::elapsed()` which respects the paused clock.
#[tokio::test(start_paused = true)]
async fn cache_put_source_with_zero_interval_reports_stale_after_interval() {
    let cache = Cache::new();
    let mut fields = HashMap::new();
    fields.insert("x".to_string(), Value::String("v".into()));
    // interval of 0 => is_stale() true when elapsed().as_secs() > 0; advance 2s to be safe.
    cache.put_source("fake", None, "main", fields, Some(0));
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    let entry = cache.get_entry("fake", None).expect("entry present");
    assert!(entry.is_stale(), "entry should be stale with interval=0");
}
