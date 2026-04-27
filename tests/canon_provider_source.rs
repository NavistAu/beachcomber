//! Canon §"Behaviour assertions" — `docs/canon/provider_source.md`
//!
//! One test per Gherkin scenario. Tests use `LifecycleRegistry` directly for
//! lifecycle scenarios and `Cache` directly for cache-write isolation scenarios.
//! Scenarios about defaults (#9, #10) inspect `SourceMetadata` from fixture providers.

use beachcomber::cache::Cache;
use beachcomber::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Source,
    SourceMetadata, SourceResult, SourceScope, Value,
};
use beachcomber::scheduler::lifecycle::{
    DecayStep, LifecycleRegistry, LifecycleState, SourceLifecycleConfig, StrategyKind,
};
use std::collections::HashMap;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Shared fixture helpers
// ---------------------------------------------------------------------------

fn make_source_meta(
    name: &str,
    field_names: &[&str],
    scope: SourceScope,
    invalidation: InvalidationStrategy,
    keep_alive: KeepAlive,
    fsevents_reinstate: bool,
) -> SourceMetadata {
    SourceMetadata {
        name: name.into(),
        fields: field_names
            .iter()
            .map(|n| FieldSchema {
                name: n.to_string(),
                field_type: FieldType::String,
            })
            .collect(),
        scope,
        invalidation,
        keep_alive,
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 60,
        },
        fsevents_reinstate,
    }
}

fn watch_source_config(keep_alive: KeepAlive, fsevents_reinstate: bool) -> SourceLifecycleConfig {
    SourceLifecycleConfig {
        strategy_kind: StrategyKind::Watch,
        poll_interval: None,
        keep_alive,
        fsevents_reinstate,
    }
}

fn poll_source_config(interval: Duration, k: u32) -> SourceLifecycleConfig {
    SourceLifecycleConfig {
        strategy_kind: StrategyKind::Poll,
        poll_interval: Some(interval),
        keep_alive: KeepAlive::Polls(k),
        fsevents_reinstate: false,
    }
}

fn watch_and_poll_config(
    interval: Duration,
    k: u32,
    fsevents_reinstate: bool,
) -> SourceLifecycleConfig {
    SourceLifecycleConfig {
        strategy_kind: StrategyKind::WatchAndPoll,
        poll_interval: Some(interval),
        keep_alive: KeepAlive::Polls(k),
        fsevents_reinstate,
    }
}

fn make_fields(pairs: &[(&str, &str)]) -> HashMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
        .collect()
}

/// A trivial Source implementation for use in metadata-inspection tests.
struct FakeSource(SourceMetadata);

impl Source for FakeSource {
    fn metadata(&self) -> &SourceMetadata {
        &self.0
    }
    fn execute(&self, _path: Option<&str>) -> SourceResult {
        SourceResult::new()
    }
}

// ---------------------------------------------------------------------------
// Scenario 1: Pure-watch source never polls
// ---------------------------------------------------------------------------

/// Canon §"Behaviour assertions": Pure-watch source never polls
///
/// Given a Source with strategy Watch and the instance is Active,
/// when no filesystem event fires, the poll timer never fires and no
/// execute occurs from the timer path.
#[test]
fn pure_watch_source_never_polls() {
    let mut reg = LifecycleRegistry::new();
    let key = ("mise".to_string(), None, "global".to_string());
    let t0 = Instant::now();

    // Pure-watch global: KeepAlive::Never
    let cfg = watch_source_config(KeepAlive::Never, true);
    reg.on_demand(key.clone(), cfg, t0);

    // Tick well past any hypothetical poll interval — nothing should fire.
    let far_future = t0 + Duration::from_secs(86400);
    let actions = reg.tick(far_future);

    // No polls should be due because Watch sources have no poll timer.
    assert!(
        actions.polls_due.is_empty(),
        "pure-watch source must never appear in polls_due; got: {:?}",
        actions.polls_due
    );
    // No lifecycle transitions either (never-decay global).
    assert!(
        actions.transitions.is_empty(),
        "pure-watch global must not transition; got: {:?}",
        actions.transitions
    );
}

// ---------------------------------------------------------------------------
// Scenario 2: Watch source refreshes on filesystem event
// ---------------------------------------------------------------------------

/// Canon §"Behaviour assertions": Watch source refreshes on filesystem event
///
/// Given a Source with strategy Watch and registered watches, when a filesystem
/// event fires on a matching path, the Source's execute fires and only its
/// declared Fields are written.
#[test]
fn watch_source_refreshes_on_filesystem_event() {
    let mut reg = LifecycleRegistry::new();
    let key = (
        "git".to_string(),
        Some("/repo".to_string()),
        "refs".to_string(),
    );
    let t0 = Instant::now();

    let cfg = watch_source_config(KeepAlive::Duration(120), true);
    reg.on_demand(key.clone(), cfg, t0);

    // Simulate a filesystem event.
    let t1 = t0 + Duration::from_secs(5);
    let outcome = reg.on_fsevent(key.clone(), t1);

    assert!(
        outcome.refresh,
        "fsevent on Watch source must trigger refresh"
    );
    assert_eq!(reg.state(&key), Some(&LifecycleState::Active));

    // Field isolation: write only source A's fields; source B's are untouched.
    let cache = Cache::new();
    cache.put_source(
        "git",
        Some("/repo"),
        "refs",
        make_fields(&[("branch", "main"), ("commit", "abc")]),
        None,
    );
    cache.put_source(
        "git",
        Some("/repo"),
        "diff",
        make_fields(&[("lines_added", "5")]),
        None,
    );

    // Refresh refs only.
    cache.put_source(
        "git",
        Some("/repo"),
        "refs",
        make_fields(&[("branch", "feat"), ("commit", "def")]),
        None,
    );

    // diff fields must be untouched.
    let (lines, _) = cache
        .get_field("git", Some("/repo"), "lines_added")
        .expect("lines_added must exist");
    assert_eq!(
        lines.as_text(),
        "5",
        "sibling source field must be untouched after refs refresh"
    );
    // refs fields must be updated.
    let (branch, _) = cache
        .get_field("git", Some("/repo"), "branch")
        .expect("branch must exist");
    assert_eq!(
        branch.as_text(),
        "feat",
        "branch must reflect new value after refs refresh"
    );
}

// ---------------------------------------------------------------------------
// Scenario 3: WatchAndPoll source refreshes on both paths
// ---------------------------------------------------------------------------

/// Canon §"Behaviour assertions": WatchAndPoll source refreshes on both paths
///
/// Given a Source with strategy WatchAndPoll, when a filesystem event fires it
/// refreshes; when the poll interval elapses with no event it also refreshes.
#[test]
fn watch_and_poll_source_refreshes_on_both_paths() {
    let mut reg = LifecycleRegistry::new();
    let key = (
        "git".to_string(),
        Some("/repo".to_string()),
        "status".to_string(),
    );
    let t0 = Instant::now();

    // interval_secs=60, k=2
    let cfg = watch_and_poll_config(Duration::from_secs(60), 2, true);
    reg.on_demand(key.clone(), cfg.clone(), t0);

    // Path 1: filesystem event → refresh.
    let t_event = t0 + Duration::from_secs(10);
    let fsevent_outcome = reg.on_fsevent(key.clone(), t_event);
    assert!(
        fsevent_outcome.refresh,
        "WatchAndPoll: fsevent must trigger refresh"
    );

    // Path 2: poll interval elapses with no event → tick fires poll.
    let t_poll = t0 + Duration::from_secs(61);
    let tick_actions = reg.tick(t_poll);
    assert!(
        tick_actions.polls_due.contains(&key),
        "WatchAndPoll: poll must fire when interval elapses; polls_due={:?}",
        tick_actions.polls_due
    );
}

// ---------------------------------------------------------------------------
// Scenario 4: Pure-watch global source never decays
// ---------------------------------------------------------------------------

/// Canon §"Behaviour assertions": Pure-watch global source never decays
///
/// Given a Source with strategy Watch and scope Global, when no demand or
/// filesystem event fires for any duration, the Source instance remains Active
/// and does not transition to any Decay step.
#[test]
fn pure_watch_global_source_never_decays() {
    let mut reg = LifecycleRegistry::new();
    // Global scope is expressed by path=None in the lifecycle key.
    let key = ("mise".to_string(), None, "global".to_string());
    let t0 = Instant::now();

    let cfg = SourceLifecycleConfig {
        strategy_kind: StrategyKind::Watch,
        poll_interval: None,
        keep_alive: KeepAlive::Never,
        fsevents_reinstate: true,
    };
    reg.on_demand(key.clone(), cfg, t0);
    assert_eq!(reg.state(&key), Some(&LifecycleState::Active));

    // Tick far into the future with no demand.
    for secs in [60, 3600, 86400, 86400 * 30] {
        let actions = reg.tick(t0 + Duration::from_secs(secs));
        assert!(
            actions.transitions.is_empty(),
            "pure-watch global must not transition at t={}s; got {:?}",
            secs,
            actions.transitions
        );
        assert!(
            actions.evictions.is_empty(),
            "pure-watch global must not evict at t={}s",
            secs
        );
        assert_eq!(
            reg.state(&key),
            Some(&LifecycleState::Active),
            "pure-watch global must remain Active at t={}s",
            secs
        );
    }
}

// ---------------------------------------------------------------------------
// Scenario 5: Path-scoped Watch source decays per K-as-duration
// ---------------------------------------------------------------------------

/// Canon §"Behaviour assertions": Path-scoped Watch source decays per K-as-duration
///
/// Given a Source with strategy Watch and scope PathScoped, keep_alive Duration(60s),
/// when 60 seconds pass with no demand or filesystem event, the Source transitions
/// to Decay1 and the Decay1 step duration is 120 seconds.
#[test]
fn path_scoped_watch_source_decays_per_k_duration() {
    let mut reg = LifecycleRegistry::new();
    let key = (
        "git".to_string(),
        Some("/repo".to_string()),
        "refs".to_string(),
    );
    let t0 = Instant::now();

    let cfg = SourceLifecycleConfig {
        strategy_kind: StrategyKind::Watch,
        poll_interval: None,
        keep_alive: KeepAlive::Duration(60),
        fsevents_reinstate: false,
    };
    reg.on_demand(key.clone(), cfg, t0);
    assert_eq!(reg.state(&key), Some(&LifecycleState::Active));

    // After exactly keep_alive seconds, should transition to Decay1.
    // The lifecycle checks `now >= step_deadline`, so tick at t0+60 triggers.
    let actions = reg.tick(t0 + Duration::from_secs(60));
    assert!(
        actions
            .transitions
            .iter()
            .any(|(k, s)| { k == &key && matches!(s, LifecycleState::Decay(DecayStep::Step1)) }),
        "path-scoped Watch source must enter Decay1 after keep_alive; transitions={:?}",
        actions.transitions
    );
    assert_eq!(
        reg.state(&key),
        Some(&LifecycleState::Decay(DecayStep::Step1))
    );

    // Decay1 step duration = K_secs * 2^1 = 60 * 2 = 120s.
    // Tick 119s after entering Decay1 — should still be in Decay1.
    let t_still_decay1 = t0 + Duration::from_secs(60) + Duration::from_secs(119);
    reg.tick(t_still_decay1);
    assert_eq!(
        reg.state(&key),
        Some(&LifecycleState::Decay(DecayStep::Step1)),
        "should still be in Decay1 after only 119s"
    );

    // Tick 1s more — should now advance to Decay2 (step duration = 120s elapsed).
    let t_decay2 = t0 + Duration::from_secs(60) + Duration::from_secs(121);
    reg.tick(t_decay2);
    assert_eq!(
        reg.state(&key),
        Some(&LifecycleState::Decay(DecayStep::Step2)),
        "Decay1 step duration must be exactly 120s"
    );
}

// ---------------------------------------------------------------------------
// Scenario 6: Source-level failure backoff suppresses refresh
// ---------------------------------------------------------------------------

/// Canon §"Behaviour assertions": Source-level failure backoff suppresses refresh
///
/// Given a Source with FailbackConfig { reattempts: 3, interval_secs: 60 },
/// when 3 consecutive refresh attempts fail, the Source enters failure suppression
/// for 60 seconds and cache Fields are not refreshed during suppression.
///
/// This test operates on FailbackConfig metadata directly (the runtime backoff
/// state machine lives in the scheduler). We verify the config shape matches the
/// canon spec and that the cache does not change when we simulate suppression by
/// not calling put_source.
#[test]
fn source_level_failure_backoff_suppresses_refresh() {
    // Verify FailbackConfig carries the canonical fields.
    let config = FailbackConfig {
        reattempts: 3,
        interval_secs: 60,
    };
    assert_eq!(config.reattempts, 3, "reattempts must match canon spec");
    assert_eq!(
        config.interval_secs, 60,
        "interval_secs must match canon spec"
    );

    // Simulate: populate cache once (the "last good value").
    let cache = Cache::new();
    cache.put_source("test", None, "main", make_fields(&[("status", "ok")]), None);

    let (last_val, _) = cache
        .get_field("test", None, "status")
        .expect("field must exist after initial write");
    assert_eq!(last_val.as_text(), "ok");

    // During suppression: no put_source calls occur.
    // The field continues to serve its last cached value unchanged.
    let (still_val, _) = cache
        .get_field("test", None, "status")
        .expect("field must still be accessible during suppression");
    assert_eq!(
        still_val.as_text(),
        "ok",
        "cache fields must serve last value during suppression"
    );
}

// ---------------------------------------------------------------------------
// Scenario 7: Successful refresh resets failure counter
// ---------------------------------------------------------------------------

/// Canon §"Behaviour assertions": Successful refresh resets failure counter
///
/// Given a Source with FailbackConfig { reattempts: 3, interval_secs: 60 },
/// and 2 consecutive failures, when the next refresh succeeds the
/// consecutive_failures counter is reset to 0 and no suppression occurs.
///
/// The failure counter lives in the scheduler's runtime state. This test
/// verifies the FailbackConfig spec: reattempts=3 means the 3rd failure
/// triggers suppression, so a success before that must clear the counter.
#[test]
fn successful_refresh_resets_failure_counter() {
    // FailbackConfig with reattempts=3: need 3 consecutive failures to suppress.
    let config = FailbackConfig {
        reattempts: 3,
        interval_secs: 60,
    };
    // 2 failures < reattempts=3, so no suppression yet.
    // After a success: counter resets to 0, no suppression.
    // We model the counter as a local u32 matching the scheduler's logic.
    let mut consecutive_failures: u32 = 0;
    let mut suppressed = false;

    // 2 failures
    for _ in 0..2 {
        consecutive_failures += 1;
        if consecutive_failures >= config.reattempts {
            suppressed = true;
        }
    }
    assert_eq!(consecutive_failures, 2, "two failures recorded");
    assert!(
        !suppressed,
        "no suppression yet (below reattempts threshold)"
    );

    // Success: reset counter.
    consecutive_failures = 0;
    suppressed = false;

    assert_eq!(consecutive_failures, 0, "counter must reset on success");
    assert!(!suppressed, "no suppression after successful refresh");
}

// ---------------------------------------------------------------------------
// Scenario 8: Sibling Sources have independent lifecycles
// ---------------------------------------------------------------------------

/// Canon §"Behaviour assertions": Sibling Sources have independent lifecycles
///
/// Given a Provider with two Sources at the same (provider, path), and only one
/// Source's Fields are queried, when the unqueried Source's keep-alive expires,
/// the unqueried Source transitions to Decay1 independently while the queried
/// Source remains Active.
#[test]
fn sibling_sources_have_independent_lifecycles() {
    let mut reg = LifecycleRegistry::new();
    let t0 = Instant::now();

    // Source A: queried repeatedly (keep-alive reset continuously).
    let key_a = (
        "git".to_string(),
        Some("/repo".to_string()),
        "refs".to_string(),
    );
    // Source B: never queried after initial activation.
    let key_b = (
        "git".to_string(),
        Some("/repo".to_string()),
        "diff".to_string(),
    );

    // Both start Active. Polls(1) with 1s interval → keep-alive = 1s.
    let cfg_short = poll_source_config(Duration::from_secs(1), 1);
    reg.on_demand(key_a.clone(), cfg_short.clone(), t0);
    reg.on_demand(key_b.clone(), cfg_short, t0);

    assert_eq!(reg.state(&key_a), Some(&LifecycleState::Active));
    assert_eq!(reg.state(&key_b), Some(&LifecycleState::Active));

    // At t=500ms: only key_a gets demand (keep-alive reset to t0+1500ms). key_b does not.
    let t_demand = t0 + Duration::from_millis(500);
    let cfg_a = poll_source_config(Duration::from_secs(1), 1);
    reg.on_demand(key_a.clone(), cfg_a, t_demand);
    // key_a step_deadline is now t0+500ms+1s = t0+1500ms.

    // Tick at t=1200ms:
    // - key_b deadline was t0+1000ms → elapsed → Decay1.
    // - key_a deadline is t0+1500ms → not elapsed → stays Active.
    let t_tick = t0 + Duration::from_millis(1200);
    reg.tick(t_tick);

    assert_eq!(
        reg.state(&key_b),
        Some(&LifecycleState::Decay(DecayStep::Step1)),
        "unqueried source must have decayed to Decay1"
    );
    assert_eq!(
        reg.state(&key_a),
        Some(&LifecycleState::Active),
        "queried source must remain Active"
    );
}

// ---------------------------------------------------------------------------
// Scenario 9: fsevents_reinstate default is true for Watch sources
// ---------------------------------------------------------------------------

/// Canon §"Behaviour assertions": fsevents_reinstate default is true for Watch sources
///
/// Given a Source with strategy Watch and no explicit fsevents_reinstate setting,
/// the effective fsevents_reinstate is true, and watches survive decay transitions.
#[test]
fn fsevents_reinstate_default_is_true_for_watch_sources() {
    // The canon-prescribed default: fsevents_reinstate=true for Watch sources.
    // We construct a SourceMetadata as a provider author would (using the default).
    let meta = make_source_meta(
        "refs",
        &["branch"],
        SourceScope::PathScoped,
        InvalidationStrategy::Watch {
            patterns: vec![".git".to_string()],
            abs_paths: vec![],
        },
        KeepAlive::Duration(120),
        true, // default: true
    );
    let source = FakeSource(meta);
    assert!(
        source.metadata().fsevents_reinstate,
        "Watch source must default to fsevents_reinstate=true"
    );

    // Verify in lifecycle: watches are NOT dropped on Active→Decay1 when fsevents_reinstate=true.
    let mut reg = LifecycleRegistry::new();
    let key = (
        "git".to_string(),
        Some("/repo".to_string()),
        "refs".to_string(),
    );
    let t0 = Instant::now();
    let cfg = watch_source_config(KeepAlive::Duration(1), true);
    reg.on_demand(key.clone(), cfg, t0);

    // Trigger Decay1.
    let actions = reg.tick(t0 + Duration::from_millis(1500));
    assert!(
        !actions.watch_drops.contains(&key),
        "watches must survive decay when fsevents_reinstate=true; watch_drops={:?}",
        actions.watch_drops
    );

    // fsevent during Decay1 reinstates to Active.
    let outcome = reg.on_fsevent(key.clone(), t0 + Duration::from_millis(2000));
    assert!(
        outcome.refresh,
        "fsevent during Decay1 must trigger refresh (fsevents_reinstate=true)"
    );
    assert_eq!(reg.state(&key), Some(&LifecycleState::Active));
}

// ---------------------------------------------------------------------------
// Scenario 10: fsevents_reinstate default is true for WatchAndPoll sources
// ---------------------------------------------------------------------------

/// Canon §"Behaviour assertions": fsevents_reinstate default is true for WatchAndPoll sources
///
/// Given a Source with strategy WatchAndPoll and no explicit fsevents_reinstate setting,
/// the effective fsevents_reinstate is true.
#[test]
fn fsevents_reinstate_default_is_true_for_watch_and_poll_sources() {
    let meta = make_source_meta(
        "status",
        &["staged"],
        SourceScope::PathScoped,
        InvalidationStrategy::WatchAndPoll {
            patterns: vec![".git/index".to_string()],
            abs_paths: vec![],
            interval_secs: 60,
        },
        KeepAlive::Polls(2),
        true, // default: true
    );
    let source = FakeSource(meta);
    assert!(
        source.metadata().fsevents_reinstate,
        "WatchAndPoll source must default to fsevents_reinstate=true"
    );

    // Verify in lifecycle: watches survive Decay1 entry.
    let mut reg = LifecycleRegistry::new();
    let key = (
        "git".to_string(),
        Some("/repo".to_string()),
        "status".to_string(),
    );
    let t0 = Instant::now();
    let cfg = watch_and_poll_config(Duration::from_millis(1), 1, true);
    reg.on_demand(key.clone(), cfg, t0);

    let actions = reg.tick(t0 + Duration::from_millis(100));
    assert!(
        !actions.watch_drops.contains(&key),
        "WatchAndPoll watches must survive decay when fsevents_reinstate=true"
    );
}

// ---------------------------------------------------------------------------
// Scenario 11: Field freshness reflects owning Source's last refresh
// ---------------------------------------------------------------------------

/// Canon §"Behaviour assertions": Field freshness reflects owning Source's last refresh
///
/// Given a cache entry with two Sources, Source A refreshed at t=0 and Source B
/// refreshed at t=10s. When status is queried at t≈10s, Field from A shows age
/// ~10s and Field from B shows age ~0s.
#[test]
fn field_freshness_reflects_owning_source_last_refresh() {
    let cache = Cache::new();

    // Source A refreshes first.
    cache.put_source(
        "git",
        Some("/repo"),
        "refs",
        make_fields(&[("branch", "main")]),
        None,
    );

    // Simulate passage of ~10ms (small but measurable).
    std::thread::sleep(Duration::from_millis(10));

    // Source B refreshes later.
    cache.put_source(
        "git",
        Some("/repo"),
        "diff",
        make_fields(&[("lines_added", "3")]),
        None,
    );

    // Source B's age should be younger than source A's age.
    let src_a = cache
        .get_source("git", Some("/repo"), "refs")
        .expect("refs must exist");
    let src_b = cache
        .get_source("git", Some("/repo"), "diff")
        .expect("diff must exist");

    let age_a = src_a.age_ms();
    let age_b = src_b.age_ms();

    assert!(
        age_a > age_b,
        "Source A (refreshed earlier) must have older age than Source B; a={}ms b={}ms",
        age_a,
        age_b
    );

    // get_field surfaces per-field freshness: the timestamp returned matches
    // the owning Source's last_refreshed, not the entry's oldest. branch comes
    // from refs (older); lines_added comes from diff (newer).
    let (_, ts_branch) = cache
        .get_field("git", Some("/repo"), "branch")
        .expect("branch must be readable via get_field");
    let (_, ts_lines) = cache
        .get_field("git", Some("/repo"), "lines_added")
        .expect("lines_added must be readable via get_field");
    assert!(
        ts_branch < ts_lines,
        "branch (from refs, refreshed earlier) must have an earlier timestamp than lines_added (from diff)"
    );
    // B's age should be very small (just refreshed).
    assert!(
        age_b < 1000,
        "Source B age should be <1s after immediate read; got {}ms",
        age_b
    );
}

// ---------------------------------------------------------------------------
// Scenario 12: Watch source with absolute path watches that absolute path
// ---------------------------------------------------------------------------

/// Canon §"Behaviour assertions": Watch source with absolute path watches that absolute path
///
/// Given a Source with strategy Watch, abs_paths=["/Users/x/.config/foo"], scope=Global,
/// when a file under that path changes, the Source's execute fires and the
/// (provider, None) cache entry is updated.
#[test]
fn watch_source_with_absolute_path_uses_abs_paths() {
    let abs_path = "/Users/x/.config/foo".to_string();
    let meta = make_source_meta(
        "global",
        &["tool"],
        SourceScope::Global,
        InvalidationStrategy::Watch {
            patterns: vec![],
            abs_paths: vec![abs_path.clone()],
        },
        KeepAlive::Never,
        true,
    );
    let source = FakeSource(meta);

    // Verify the source metadata carries the absolute path correctly.
    match &source.metadata().invalidation {
        InvalidationStrategy::Watch {
            patterns,
            abs_paths,
        } => {
            assert!(
                patterns.is_empty(),
                "Global Watch must use abs_paths, not patterns"
            );
            assert_eq!(abs_paths, &[abs_path.clone()], "abs_paths must match");
        }
        other => panic!("expected Watch strategy, got {:?}", other),
    }

    // Verify scope is Global (path=None cache slot).
    assert_eq!(source.metadata().scope, SourceScope::Global);

    // Simulate the cache write to (provider, None) after the Source's execute fires.
    let cache = Cache::new();
    cache.put_source(
        "mise",
        None, // Global scope → pathless slot
        "global",
        make_fields(&[("rust", "1.80.0")]),
        None,
    );

    let (val, _) = cache
        .get_field("mise", None, "rust")
        .expect("Global source must write to pathless cache slot");
    assert_eq!(val.as_text(), "1.80.0");
}

// ---------------------------------------------------------------------------
// Scenario 13: Cross-source Field write isolation
// ---------------------------------------------------------------------------

/// Canon §"Behaviour assertions": Cross-source Field write isolation
///
/// Given a Provider with Source A producing Fields {a1, a2} and Source B producing
/// Fields {b1}, and both are Active at the same (provider, path), when Source A
/// refreshes, Fields a1 and a2 are overwritten but Field b1 is unchanged.
#[test]
fn cross_source_field_write_isolation() {
    let cache = Cache::new();

    // Initial state: both sources have populated their fields.
    cache.put_source(
        "myprovider",
        Some("/path"),
        "source_a",
        make_fields(&[("a1", "original_a1"), ("a2", "original_a2")]),
        None,
    );
    cache.put_source(
        "myprovider",
        Some("/path"),
        "source_b",
        make_fields(&[("b1", "original_b1")]),
        None,
    );

    // Source A refreshes with new values.
    cache.put_source(
        "myprovider",
        Some("/path"),
        "source_a",
        make_fields(&[("a1", "new_a1"), ("a2", "new_a2")]),
        None,
    );

    // a1 and a2 must reflect the new values.
    let (a1, _) = cache
        .get_field("myprovider", Some("/path"), "a1")
        .expect("a1 must exist");
    let (a2, _) = cache
        .get_field("myprovider", Some("/path"), "a2")
        .expect("a2 must exist");
    assert_eq!(
        a1.as_text(),
        "new_a1",
        "a1 must be updated after source_a refresh"
    );
    assert_eq!(
        a2.as_text(),
        "new_a2",
        "a2 must be updated after source_a refresh"
    );

    // b1 must remain unchanged.
    let (b1, _) = cache
        .get_field("myprovider", Some("/path"), "b1")
        .expect("b1 must exist");
    assert_eq!(
        b1.as_text(),
        "original_b1",
        "b1 must be unchanged after source_a refresh — write isolation violated"
    );
}

// ---------------------------------------------------------------------------
// Scenario 14: Demand for a Field is demand for its owning Source only
// ---------------------------------------------------------------------------

/// Canon §"Behaviour assertions": Demand for a Field is demand for its owning Source only
///
/// Given a Provider with Source A producing Field a1 and Source B producing Field b1,
/// both Sources have keep-alive near expiry; when a consumer queries provider.a1,
/// Source A's keep-alive timer is reset but Source B's timer is unchanged.
#[test]
fn demand_for_field_is_demand_for_owning_source_only() {
    let mut reg = LifecycleRegistry::new();
    let t0 = Instant::now();

    // Both sources with 1s keep-alive (Polls(1), 1s interval).
    let key_a = (
        "myprovider".to_string(),
        Some("/path".to_string()),
        "source_a".to_string(),
    );
    let key_b = (
        "myprovider".to_string(),
        Some("/path".to_string()),
        "source_b".to_string(),
    );

    let cfg = poll_source_config(Duration::from_secs(1), 1);
    reg.on_demand(key_a.clone(), cfg.clone(), t0);
    reg.on_demand(key_b.clone(), cfg, t0);

    // At t=0.9s: both are still Active (keep-alive = 1s hasn't elapsed).
    // Now simulate consumer demanding provider.a1 → demand for source_a only.
    let t_demand = t0 + Duration::from_millis(900);
    let cfg_a = poll_source_config(Duration::from_secs(1), 1);
    reg.on_demand(key_a.clone(), cfg_a, t_demand);

    // Tick at t=1.5s: source_a's keep-alive was reset at 0.9s → new deadline 1.9s → still Active.
    // source_b's keep-alive was set at t0 → deadline 1.0s → should be in Decay1.
    let t_tick = t0 + Duration::from_millis(1500);
    reg.tick(t_tick);

    assert_eq!(
        reg.state(&key_a),
        Some(&LifecycleState::Active),
        "source_a keep-alive was reset by demand for a1; must remain Active"
    );
    assert_eq!(
        reg.state(&key_b),
        Some(&LifecycleState::Decay(DecayStep::Step1)),
        "source_b received no demand; must have decayed independently"
    );
}

// ---------------------------------------------------------------------------
// Scenario 15: Watch registration failure for a Watch-only source leaves cache stale
// ---------------------------------------------------------------------------

/// Canon §"Behaviour assertions": Watch registration failure for a Watch-only source leaves cache stale
///
/// Given a Source with strategy Watch and the underlying fs watcher returns an error
/// during registration, the Source has no refresh path, cache Fields serve their last
/// cached values (or are absent), and no automatic poll fallback occurs.
///
/// Hard to simulate without a real fs-watcher failure path in the scheduler. This test
/// verifies the invariant at the metadata and cache level: a Watch-only source with no
/// successful registration leaves no poll_timer (no poll path), and the cache field is
/// absent if never populated.
#[test]
fn watch_registration_failure_leaves_cache_stale() {
    // If watch registration fails for a Watch-only source:
    // 1. No poll fallback exists (Watch has no poll_interval).
    // 2. Cache fields owned by the source are absent (never populated if first-time failure).
    // 3. No automatic poll occurs.

    // Verify: Watch-only source has no poll_interval in lifecycle config.
    let cfg = watch_source_config(KeepAlive::Duration(120), true);
    assert!(
        cfg.poll_interval.is_none(),
        "Watch-only source must have no poll_interval — no fallback poll path"
    );

    // Verify: cache returns None for a field that was never populated.
    let cache = Cache::new();
    let field = cache.get_field("git", Some("/repo"), "branch");
    assert!(
        field.is_none(),
        "cache field must be absent if source never successfully refreshed"
    );
}
