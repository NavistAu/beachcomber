use beachcomber::cache::Cache;
use beachcomber::provider::{InvalidationStrategy, ProviderResult, Value, expected_interval_secs};

#[test]
fn expected_interval_secs_maps_all_strategies() {
    assert_eq!(
        expected_interval_secs(&InvalidationStrategy::Poll {
            interval_secs: 30,
            floor_secs: 1
        }),
        Some(30)
    );
    assert_eq!(
        expected_interval_secs(&InvalidationStrategy::WatchAndPoll {
            patterns: vec![".git".into()],
            interval_secs: 60,
            floor_secs: 1,
        }),
        Some(60)
    );
    assert_eq!(
        expected_interval_secs(&InvalidationStrategy::Watch {
            patterns: vec![".git".into()],
            fallback_poll_secs: Some(90),
        }),
        Some(90)
    );
    assert_eq!(
        expected_interval_secs(&InvalidationStrategy::Watch {
            patterns: vec![".git".into()],
            fallback_poll_secs: None,
        }),
        None
    );
    assert_eq!(expected_interval_secs(&InvalidationStrategy::Once), None);
}

#[test]
fn cache_put_with_interval_reports_stale_after_interval() {
    let cache = Cache::new();
    let mut r = ProviderResult::new();
    r.insert("x", Value::String("v".into()));
    cache.put_with_interval("fake", None, r, Some(0)); // interval of 0 => stale right after creation
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let entry = cache.get("fake", None).expect("entry present");
    assert!(entry.is_stale(), "entry should be stale with interval=0");
}
