/// Sanity test verifying that cache staleness respects tokio's mock clock.
///
/// After this refactor, `CacheSourceEntry::is_stale()` uses `tokio::time::Instant`,
/// so advancing tokio's mock clock causes previously-fresh entries to flip to stale.
/// This is the prerequisite for P5.11 (replacing sleep-based test waits).
use beachcomber::cache::Cache;
use beachcomber::provider::Value;
use std::collections::HashMap;

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
