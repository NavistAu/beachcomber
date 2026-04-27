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
#[test]
fn expired_entry_is_stale() {
    let cache = Cache::new();
    let mut fields = HashMap::new();
    fields.insert("value".to_string(), Value::String("hello".to_string()));

    // Interval of 0 seconds means the entry is immediately stale after any elapsed time.
    cache.put_source("myprov", None, "main", fields, Some(0));

    // Sleep just enough to ensure elapsed > 0 seconds (i.e. >= 1s is not needed;
    // elapsed().as_secs() > 0 requires at least 1s, so we use a 1ms sleep but
    // the check is > interval_secs, so with interval=0 even 1ms elapsed gives
    // elapsed.as_secs()=0 which is NOT > 0. Use interval=0 and rely on:
    // is_stale checks elapsed.as_secs() > interval, so we need elapsed >= 1s.
    // Instead, use interval of 0 and sleep 1100ms to guarantee staleness.
    std::thread::sleep(std::time::Duration::from_millis(1100));

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
