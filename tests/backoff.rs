// Tests for the lifecycle state machine that replaced the old BackoffStage/BackoffState.
// The old backoff machinery (Grace/SlowPoll/Frozen/Evict) has been replaced by
// LifecycleRegistry with Active and Decay1..4 states. These tests exercise the
// same behavioral contracts via the new API.

use beachcomber::scheduler::lifecycle::{
    DecayStep, LifecycleRegistry, LifecycleState, ProviderLifecycleConfig, StateTransition,
};
use std::time::{Duration, Instant};

fn test_config() -> ProviderLifecycleConfig {
    ProviderLifecycleConfig {
        poll_interval: Duration::from_secs(0), // instant poll for tests
        keep_alive_polls: 0,                   // instant decay for tests
        fsevents_reinstate: false,
    }
}

#[test]
fn lifecycle_starts_in_active() {
    let mut reg = LifecycleRegistry::new();
    let key = ("git".to_string(), None);
    let t0 = Instant::now();

    reg.on_demand(key.clone(), test_config(), t0);

    assert_eq!(reg.state(&key), Some(&LifecycleState::Active));
}

#[test]
fn lifecycle_advances_through_decay_stages() {
    let mut reg = LifecycleRegistry::new();
    let key = ("git".to_string(), None);
    let t0 = Instant::now();
    reg.on_demand(key.clone(), test_config(), t0);

    // With poll_interval=0 and keep_alive_polls=0, tick immediately advances state.
    // Active → Decay1
    let t1 = t0 + Duration::from_millis(1);
    reg.tick(t1);
    assert_eq!(
        reg.state(&key),
        Some(&LifecycleState::Decay(DecayStep::Step1))
    );

    // Decay1 → Decay2
    let t2 = t1 + Duration::from_millis(1);
    reg.tick(t2);
    assert_eq!(
        reg.state(&key),
        Some(&LifecycleState::Decay(DecayStep::Step2))
    );

    // Decay2 → Decay3
    let t3 = t2 + Duration::from_millis(1);
    reg.tick(t3);
    assert_eq!(
        reg.state(&key),
        Some(&LifecycleState::Decay(DecayStep::Step3))
    );

    // Decay3 → Decay4
    let t4 = t3 + Duration::from_millis(1);
    reg.tick(t4);
    assert_eq!(
        reg.state(&key),
        Some(&LifecycleState::Decay(DecayStep::Step4))
    );
}

#[test]
fn lifecycle_resets_to_active_on_demand() {
    let mut reg = LifecycleRegistry::new();
    let key = ("git".to_string(), None);
    let t0 = Instant::now();
    reg.on_demand(key.clone(), test_config(), t0);

    // Advance into decay.
    reg.tick(t0 + Duration::from_millis(1));
    assert_eq!(
        reg.state(&key),
        Some(&LifecycleState::Decay(DecayStep::Step1))
    );

    // New demand reinstates to Active.
    let t2 = t0 + Duration::from_millis(2);
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
    let key = ("git".to_string(), None);
    let t0 = Instant::now();
    reg.on_demand(key.clone(), test_config(), t0);

    // Tick through Active + 4 decay steps = 5 ticks minimum.
    let mut t = t0 + Duration::from_millis(1);
    for _ in 0..5 {
        reg.tick(t);
        t += Duration::from_millis(1);
    }

    // After Decay4 expiry, entry should be evicted (None = not in registry).
    assert!(
        reg.state(&key).is_none(),
        "entry should be evicted after decay4 completes"
    );
}
