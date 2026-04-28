use beachcomber::cache::Cache;
use beachcomber::provider::Value;
use std::collections::HashMap;

/// Test 1: A fresh entry with an interval is not stale.
#[test]
fn fresh_entry_is_not_stale() {
    let cache = Cache::new();
    let mut fields = HashMap::new();
    fields.insert("value".to_string(), Value::String("hello".to_string()));

    // Put with a 60-second interval — entry is brand new, should not be stale.
    cache.put_source("myprov", None, "main", fields, Some(60));

    let entry = cache.get_entry("myprov", None).unwrap();
    assert!(
        !entry.is_stale(),
        "Freshly written entry should not be stale"
    );
}

/// Test 2: An entry whose age exceeds the interval is stale.
/// We use an interval of 0 seconds so any elapsed time makes it stale.
///
/// Uses tokio mock clock: `start_paused = true` + `advance` avoids a real 1.1 s wall-clock wait.
/// `is_stale()` calls `tokio::time::Instant::elapsed()` which respects the paused clock.
#[tokio::test(start_paused = true)]
async fn expired_entry_is_stale() {
    let cache = Cache::new();
    let mut fields = HashMap::new();
    fields.insert("value".to_string(), Value::String("hello".to_string()));

    // Interval of 0 seconds: is_stale() returns true when elapsed().as_secs() > 0.
    cache.put_source("myprov", None, "main", fields, Some(0));

    // Advance mock clock by 2 seconds — as_secs() truncates so 1s may give 0; 2s is safe.
    tokio::time::advance(std::time::Duration::from_secs(2)).await;

    let entry = cache.get_entry("myprov", None).unwrap();
    assert!(
        entry.is_stale(),
        "Entry older than its interval should be stale"
    );
}

/// Test 3: An entry stored with no interval is never stale.
#[test]
fn no_interval_entry_is_never_stale() {
    let cache = Cache::new();
    let mut fields = HashMap::new();
    fields.insert("value".to_string(), Value::String("hello".to_string()));

    // put_source with None interval — no staleness tracking.
    cache.put_source("myprov", None, "main", fields, None);

    let entry = cache.get_entry("myprov", None).unwrap();
    assert!(
        !entry.is_stale(),
        "Entry with no interval should never be stale"
    );
}
