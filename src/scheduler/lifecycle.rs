//! Cache entry lifecycle state machine.
//!
//! Executable form of the behaviour defined in `docs/cache-lifecycle.md`.
//! State transitions: Cold → Active → Decay1..4 → Evicted. Per-entry state
//! held in `LifecycleEntry`; scheduler dispatches on demand/fsevent/tick
//! via `LifecycleRegistry`.

// Fields and methods are exercised progressively in Tasks 3-7; dead_code
// warnings are expected until then and suppressed here rather than per-item.
#![allow(dead_code)]

use std::collections::HashMap;
use std::time::{Duration, Instant};

pub type Key = (String, Option<String>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleState {
    Active,
    Decay(DecayStep),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecayStep {
    Step1,
    Step2,
    Step3,
    Step4,
}

impl DecayStep {
    /// 1 through 4.
    pub fn as_u8(self) -> u8 {
        match self {
            DecayStep::Step1 => 1,
            DecayStep::Step2 => 2,
            DecayStep::Step3 => 3,
            DecayStep::Step4 => 4,
        }
    }

    /// Poll rate multiplier relative to P: 2^n.
    pub fn rate_multiplier(self) -> u32 {
        1u32 << self.as_u8()
    }

    /// Next step, or None if Step4 (next is Evicted, handled by caller).
    pub fn next(self) -> Option<DecayStep> {
        match self {
            DecayStep::Step1 => Some(DecayStep::Step2),
            DecayStep::Step2 => Some(DecayStep::Step3),
            DecayStep::Step3 => Some(DecayStep::Step4),
            DecayStep::Step4 => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PollTimer {
    pub last_fired: Instant,
    pub interval: Duration,
}

#[derive(Debug, Clone)]
pub struct DecayTimer {
    pub last_demand: Instant,
    pub step_deadline: Instant,
}

#[derive(Debug, Clone)]
pub struct ProviderLifecycleConfig {
    pub poll_interval: Duration,
    pub keep_alive_polls: u32,
    pub fsevents_reinstate: bool,
}

#[derive(Debug, Clone)]
pub struct LifecycleEntry {
    pub state: LifecycleState,
    pub poll_timer: PollTimer,
    pub decay_timer: DecayTimer,
    pub config: ProviderLifecycleConfig,
}

pub struct LifecycleRegistry {
    entries: HashMap<Key, LifecycleEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateTransition {
    NewlyActive,
    ResetKeepAlive,
    Reinstated { from: DecayStep },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchAction {
    Register,
    Preserve,
    Reinstate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemandOutcome {
    pub transition: StateTransition,
    pub watch_registration: WatchAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FseventOutcome {
    pub transition: Option<StateTransition>,
    pub refresh: bool,
}

#[derive(Debug, Clone, Default)]
pub struct TickActions {
    pub polls_due: Vec<Key>,
    pub transitions: Vec<(Key, LifecycleState)>,
    pub watch_drops: Vec<Key>,
    pub evictions: Vec<Key>,
}

impl LifecycleRegistry {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    pub fn on_demand(
        &mut self,
        _key: Key,
        _config: ProviderLifecycleConfig,
        _now: Instant,
    ) -> DemandOutcome {
        unimplemented!("Task 3")
    }

    pub fn on_fsevent(&mut self, _key: Key, _now: Instant) -> FseventOutcome {
        unimplemented!("Task 4")
    }

    pub fn tick(&mut self, _now: Instant) -> TickActions {
        unimplemented!("Tasks 5-7")
    }

    pub fn poll_interval(&self, key: &Key) -> Option<Duration> {
        self.entries.get(key).map(|e| e.poll_timer.interval)
    }

    pub fn state(&self, key: &Key) -> Option<&LifecycleState> {
        self.entries.get(key).map(|e| &e.state)
    }

    pub fn remove(&mut self, key: &Key) {
        self.entries.remove(key);
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Key, &LifecycleEntry)> {
        self.entries.iter()
    }
}

impl Default for LifecycleRegistry {
    fn default() -> Self {
        Self::new()
    }
}
