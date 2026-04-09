use beachcomber::scheduler::{BackoffStage, BackoffState};
use std::time::Duration;

#[test]
fn backoff_starts_in_grace() {
    let state = BackoffState::new(Duration::from_secs(30));
    assert!(matches!(state.stage(), BackoffStage::Grace));
}

#[test]
fn backoff_advances_through_stages() {
    let mut state = BackoffState::new(Duration::from_secs(0));
    state.advance();
    assert!(matches!(state.stage(), BackoffStage::SlowPoll));
    state.advance();
    assert!(matches!(state.stage(), BackoffStage::Frozen));
    state.advance();
    assert!(matches!(state.stage(), BackoffStage::Evict));
}

#[test]
fn backoff_resets() {
    let mut state = BackoffState::new(Duration::from_secs(0));
    state.advance();
    state.advance();
    state.reset(Duration::from_secs(30));
    assert!(matches!(state.stage(), BackoffStage::Grace));
}

#[test]
fn backoff_should_watch() {
    let state = BackoffState::new(Duration::from_secs(0));
    assert!(state.should_watch(), "Grace keeps watches");

    let mut state = BackoffState::new(Duration::from_secs(0));
    state.advance();
    assert!(!state.should_watch(), "SlowPoll drops watches");
}
