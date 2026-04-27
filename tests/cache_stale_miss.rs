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

#[test]
fn cache_put_source_with_zero_interval_reports_stale_after_interval() {
    let cache = Cache::new();
    let mut fields = HashMap::new();
    fields.insert("x".to_string(), Value::String("v".into()));
    // interval of 0 => immediately stale
    cache.put_source("fake", None, "main", fields, Some(0));
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let entry = cache.get_entry("fake", None).expect("entry present");
    assert!(entry.is_stale(), "entry should be stale with interval=0");
}
