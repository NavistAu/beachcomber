use crate::cache::Cache;
use crate::provider::{InvalidationStrategy, Value, expected_interval_secs};
use std::collections::HashMap;

// ── from tests/suite/staleness.rs ──────────

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

// ── from tests/suite/cache_stale_miss.rs ──────────

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

// ── from tests/suite/cache_clock.rs ──────────

/// Sanity test verifying that cache staleness respects tokio's mock clock.
///
/// After this refactor, `CacheSourceEntry::is_stale()` uses `tokio::time::Instant`,
/// so advancing tokio's mock clock causes previously-fresh entries to flip to stale.
/// This is the prerequisite for P5.11 (replacing sleep-based test waits).
fn make_fields(pairs: &[(&str, &str)]) -> HashMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
        .collect()
}

#[tokio::test(start_paused = true)]
async fn cache_entry_becomes_stale_after_advance() {
    let cache = Cache::new();

    // Insert an entry with a 30-second TTL (interval_secs = 30).
    cache.put_source(
        "git",
        Some("/repo"),
        "base",
        make_fields(&[("branch", "main")]),
        Some(30),
    );

    // Immediately after insertion the entry must not be stale.
    let entry = cache
        .get_entry("git", Some("/repo"))
        .expect("entry must exist");
    assert!(
        !entry.is_stale(),
        "entry must not be stale immediately after insertion"
    );

    // Advance mock clock by 31 seconds — past the TTL.
    tokio::time::advance(tokio::time::Duration::from_secs(31)).await;

    let entry = cache
        .get_entry("git", Some("/repo"))
        .expect("entry must still exist");
    assert!(
        entry.is_stale(),
        "entry must be stale after advancing mock clock past TTL"
    );
}

#[tokio::test(start_paused = true)]
async fn cache_entry_age_ms_advances_with_mock_clock() {
    let cache = Cache::new();

    cache.put_source(
        "hostname",
        None,
        "main",
        make_fields(&[("name", "box1")]),
        None,
    );

    let before = cache
        .get_entry("hostname", None)
        .expect("entry must exist")
        .age_ms();

    tokio::time::advance(tokio::time::Duration::from_secs(5)).await;

    let after = cache
        .get_entry("hostname", None)
        .expect("entry must exist")
        .age_ms();

    assert!(
        after >= before + 5_000,
        "age_ms must increase by at least 5000ms after advancing clock 5s (before={before}, after={after})"
    );
}

#[tokio::test(start_paused = true)]
async fn cache_entry_without_ttl_never_stale() {
    let cache = Cache::new();

    cache.put_source(
        "hostname",
        None,
        "main",
        make_fields(&[("name", "box1")]),
        None, // no interval → is_stale() always returns false
    );

    tokio::time::advance(tokio::time::Duration::from_secs(3600)).await;

    let entry = cache.get_entry("hostname", None).expect("entry must exist");
    assert!(
        !entry.is_stale(),
        "entry with no TTL must never be stale regardless of elapsed time"
    );
}

/// Each put address carries its own stamp: writing `ns.b` does not reset the
/// age of `ns.a`.
#[tokio::test(start_paused = true)]
async fn put_addresses_keep_their_own_age() {
    use crate::cache::PUT_SOURCE;
    let cache = Cache::new();
    let registry = crate::provider::registry::ProviderRegistry::new();
    let put = |node: &str, v: i64| {
        cache
            .write_static("ns", None, PUT_SOURCE, node, Value::Int(v), None)
            .unwrap();
    };
    put("a", 1);
    tokio::time::advance(std::time::Duration::from_secs(5)).await;
    put("b", 2);

    let read = cache
        .read_merged("ns", None, None, &registry, crate::cache::Layers::All)
        .unwrap();
    assert_eq!(read.records["a"].age_ms(), 5000);
    assert_eq!(read.records["b"].age_ms(), 0);
}
