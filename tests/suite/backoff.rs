// Tests for the lifecycle state machine that replaced the old BackoffStage/BackoffState.
// The old backoff machinery (Grace/SlowPoll/Frozen/Evict) has been replaced by
// LifecycleRegistry with Active and Decay1..4 states. These tests exercise the
// same behavioral contracts via the new API.

use beachcomber::provider::KeepAlive;
use beachcomber::scheduler::lifecycle::{
    DecayStep, LifecycleRegistry, LifecycleState, SourceLifecycleConfig, StateTransition,
    StrategyKind,
};
use std::time::{Duration, Instant};

fn test_config() -> SourceLifecycleConfig {
    // 1ms poll + Polls(1) keep-alive = 1ms per step for fast test progression.
    SourceLifecycleConfig {
        strategy_kind: StrategyKind::Poll,
        poll_interval: Some(Duration::from_millis(1)),
        keep_alive: KeepAlive::Polls(1),
        fsevents_reinstate: false,
    }
}

#[test]
fn lifecycle_starts_in_active() {
    let mut reg = LifecycleRegistry::new();
    let key = ("git".to_string(), None, "refs".to_string());
    let t0 = Instant::now();

    reg.on_demand(key.clone(), test_config(), t0);

    assert_eq!(reg.state(&key), Some(&LifecycleState::Active));
}

// With `poll_interval=1ms, keep_alive=Polls(1)`, step durations double per
// decay step (2^n × P × K): {1, 2, 4, 8, 16} ms for Active→Decay1…Decay4.
// Advancing 100 ms per tick is comfortably past every deadline.
const STEP: Duration = Duration::from_millis(100);

#[test]
fn lifecycle_advances_through_decay_stages() {
    let mut reg = LifecycleRegistry::new();
    let key = ("git".to_string(), None, "refs".to_string());
    let t0 = Instant::now();
    reg.on_demand(key.clone(), test_config(), t0);

    let t1 = t0 + STEP;
    reg.tick(t1);
    assert_eq!(
        reg.state(&key),
        Some(&LifecycleState::Decay(DecayStep::Step1))
    );

    let t2 = t1 + STEP;
    reg.tick(t2);
    assert_eq!(
        reg.state(&key),
        Some(&LifecycleState::Decay(DecayStep::Step2))
    );

    let t3 = t2 + STEP;
    reg.tick(t3);
    assert_eq!(
        reg.state(&key),
        Some(&LifecycleState::Decay(DecayStep::Step3))
    );

    let t4 = t3 + STEP;
    reg.tick(t4);
    assert_eq!(
        reg.state(&key),
        Some(&LifecycleState::Decay(DecayStep::Step4))
    );
}

#[test]
fn lifecycle_resets_to_active_on_demand() {
    let mut reg = LifecycleRegistry::new();
    let key = ("git".to_string(), None, "refs".to_string());
    let t0 = Instant::now();
    reg.on_demand(key.clone(), test_config(), t0);

    // Advance into decay.
    reg.tick(t0 + STEP);
    assert_eq!(
        reg.state(&key),
        Some(&LifecycleState::Decay(DecayStep::Step1))
    );

    // New demand reinstates to Active.
    let t2 = t0 + STEP * 2;
    let outcome = reg.on_demand(key.clone(), test_config(), t2);
    assert!(matches!(
        outcome.transition,
        StateTransition::Reinstated {
            from: DecayStep::Step1
        }
    ));
    assert_eq!(reg.state(&key), Some(&LifecycleState::Active));
}

#[test]
fn lifecycle_evicts_after_decay4() {
    let mut reg = LifecycleRegistry::new();
    let key = ("git".to_string(), None, "refs".to_string());
    let t0 = Instant::now();
    reg.on_demand(key.clone(), test_config(), t0);

    // Tick through Active + 4 decay steps = 5 ticks past all deadlines.
    let mut t = t0 + STEP;
    for _ in 0..5 {
        reg.tick(t);
        t += STEP;
    }

    // After Decay4 expiry, entry should be evicted (None = not in registry).
    assert!(
        reg.state(&key).is_none(),
        "entry should be evicted after decay4 completes"
    );
}
