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
        key: Key,
        config: ProviderLifecycleConfig,
        now: Instant,
    ) -> DemandOutcome {
        let keep_alive_duration = config.poll_interval * config.keep_alive_polls;

        match self.entries.get_mut(&key) {
            None => {
                // Cold → Active: create entry.
                let entry = LifecycleEntry {
                    state: LifecycleState::Active,
                    poll_timer: PollTimer {
                        last_fired: now,
                        interval: config.poll_interval,
                    },
                    decay_timer: DecayTimer {
                        last_demand: now,
                        step_deadline: now + keep_alive_duration,
                    },
                    config,
                };
                self.entries.insert(key, entry);
                DemandOutcome {
                    transition: StateTransition::NewlyActive,
                    watch_registration: WatchAction::Register,
                }
            }
            Some(entry) => match entry.state {
                LifecycleState::Active => {
                    // Active → Active: bump decay timer.
                    entry.decay_timer.last_demand = now;
                    entry.decay_timer.step_deadline = now + keep_alive_duration;
                    entry.config = config;
                    DemandOutcome {
                        transition: StateTransition::ResetKeepAlive,
                        watch_registration: WatchAction::Preserve,
                    }
                }
                LifecycleState::Decay(step) => {
                    // Decay → Active: reset everything. Whether watches were
                    // dropped depends on the PAST config (fsevents_reinstate
                    // at the time we entered decay). We consult the current
                    // entry.config before overwriting it.
                    let was_keep_during_decay = entry.config.fsevents_reinstate;
                    entry.state = LifecycleState::Active;
                    entry.poll_timer.last_fired = now;
                    entry.poll_timer.interval = config.poll_interval;
                    entry.decay_timer.last_demand = now;
                    entry.decay_timer.step_deadline = now + keep_alive_duration;
                    entry.config = config;
                    DemandOutcome {
                        transition: StateTransition::Reinstated { from: step },
                        watch_registration: if was_keep_during_decay {
                            WatchAction::Preserve
                        } else {
                            WatchAction::Reinstate
                        },
                    }
                }
            },
        }
    }

    pub fn on_fsevent(&mut self, key: Key, now: Instant) -> FseventOutcome {
        let Some(entry) = self.entries.get(&key) else {
            return FseventOutcome {
                transition: None,
                refresh: false,
            };
        };

        let is_active = matches!(entry.state, LifecycleState::Active);
        let reinstate_allowed = entry.config.fsevents_reinstate;

        if is_active || reinstate_allowed {
            // Treat as demand. Clone config (on_demand consumes it).
            let config = entry.config.clone();
            let outcome = self.on_demand(key, config, now);
            FseventOutcome {
                transition: Some(outcome.transition),
                refresh: true,
            }
        } else {
            // In Decay with fsevents_reinstate=false — ignore idempotently.
            FseventOutcome {
                transition: None,
                refresh: false,
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key(provider: &str, path: &str) -> Key {
        (provider.to_string(), Some(path.to_string()))
    }

    fn test_config() -> ProviderLifecycleConfig {
        ProviderLifecycleConfig {
            poll_interval: Duration::from_secs(60),
            keep_alive_polls: 12,
            fsevents_reinstate: false,
        }
    }

    /// Scenario: Cold cache miss triggers inline fetch.
    /// on_demand with no existing entry creates an Active entry.
    #[test]
    fn on_demand_cold_creates_active_entry() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key("git", "/repo");
        let t0 = Instant::now();

        let outcome = reg.on_demand(key.clone(), test_config(), t0);

        assert_eq!(outcome.transition, StateTransition::NewlyActive);
        assert_eq!(outcome.watch_registration, WatchAction::Register);
        assert_eq!(reg.state(&key), Some(&LifecycleState::Active));
        assert_eq!(reg.poll_interval(&key), Some(Duration::from_secs(60)));
    }

    /// Scenario: Warm read resets keep-alive.
    #[test]
    fn on_demand_on_active_resets_keep_alive() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key("git", "/repo");
        let t0 = Instant::now();
        let cfg = test_config();

        reg.on_demand(key.clone(), cfg.clone(), t0);
        let t1 = t0 + Duration::from_secs(100);

        let outcome = reg.on_demand(key.clone(), cfg, t1);

        assert_eq!(outcome.transition, StateTransition::ResetKeepAlive);
        assert_eq!(outcome.watch_registration, WatchAction::Preserve);
        assert_eq!(reg.state(&key), Some(&LifecycleState::Active));

        let entry = reg.entries.get(&key).expect("entry exists");
        assert_eq!(entry.decay_timer.last_demand, t1);
    }

    /// Scenario: Consumer request in a decay state reinstates to Active.
    /// Uses white-box mutation to put the entry in Decay2 without running tick().
    #[test]
    fn on_demand_on_decay_reinstates_to_active() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key("git", "/repo");
        let t0 = Instant::now();
        let cfg = test_config();

        reg.on_demand(key.clone(), cfg.clone(), t0);

        // Force the entry into Decay2 by mutating directly.
        {
            let entry = reg.entries.get_mut(&key).expect("entry exists");
            entry.state = LifecycleState::Decay(DecayStep::Step2);
            entry.poll_timer.interval = Duration::from_secs(240);
        }

        let t1 = t0 + Duration::from_secs(500);
        let outcome = reg.on_demand(key.clone(), cfg, t1);

        assert_eq!(
            outcome.transition,
            StateTransition::Reinstated {
                from: DecayStep::Step2
            }
        );
        assert_eq!(outcome.watch_registration, WatchAction::Reinstate);
        assert_eq!(reg.state(&key), Some(&LifecycleState::Active));
        assert_eq!(reg.poll_interval(&key), Some(Duration::from_secs(60)));

        let entry = reg.entries.get(&key).expect("entry exists");
        assert_eq!(entry.decay_timer.last_demand, t1);
    }

    /// Reinstatement when fsevents_reinstate = true: watches were preserved,
    /// so watch_registration is Preserve (not Reinstate).
    #[test]
    fn on_demand_reinstatement_preserves_watches_when_fsevents_reinstate() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key("mise", "/repo");
        let t0 = Instant::now();
        let cfg = ProviderLifecycleConfig {
            fsevents_reinstate: true,
            ..test_config()
        };

        reg.on_demand(key.clone(), cfg.clone(), t0);

        {
            let entry = reg.entries.get_mut(&key).expect("entry exists");
            entry.state = LifecycleState::Decay(DecayStep::Step3);
        }

        let t1 = t0 + Duration::from_secs(1000);
        let outcome = reg.on_demand(key.clone(), cfg, t1);

        assert_eq!(outcome.watch_registration, WatchAction::Preserve);
    }

    /// fsevent while Active: treated as demand, refresh triggered.
    #[test]
    fn on_fsevent_on_active_triggers_refresh_and_resets() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key("git", "/repo");
        let t0 = Instant::now();

        reg.on_demand(key.clone(), test_config(), t0);
        let t1 = t0 + Duration::from_secs(30);

        let outcome = reg.on_fsevent(key.clone(), t1);

        assert!(outcome.refresh);
        assert!(matches!(
            outcome.transition,
            Some(StateTransition::ResetKeepAlive)
        ));

        let entry = reg.entries.get(&key).expect("entry exists");
        assert_eq!(entry.decay_timer.last_demand, t1);
    }

    /// fsevent while decaying with fsevents_reinstate = true: reinstates to Active.
    #[test]
    fn on_fsevent_on_decay_with_fsevents_reinstate_true_reinstates() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key("mise", "/repo");
        let t0 = Instant::now();
        let cfg = ProviderLifecycleConfig {
            fsevents_reinstate: true,
            ..test_config()
        };

        reg.on_demand(key.clone(), cfg, t0);

        {
            let entry = reg.entries.get_mut(&key).expect("entry exists");
            entry.state = LifecycleState::Decay(DecayStep::Step3);
        }

        let t1 = t0 + Duration::from_secs(1000);
        let outcome = reg.on_fsevent(key.clone(), t1);

        assert!(outcome.refresh);
        assert!(matches!(
            outcome.transition,
            Some(StateTransition::Reinstated {
                from: DecayStep::Step3
            })
        ));
        assert_eq!(reg.state(&key), Some(&LifecycleState::Active));
    }

    /// fsevent while decaying with fsevents_reinstate = false: ignored.
    #[test]
    fn on_fsevent_on_decay_with_fsevents_reinstate_false_is_ignored() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key("git", "/repo");
        let t0 = Instant::now();
        let cfg = test_config(); // fsevents_reinstate = false

        reg.on_demand(key.clone(), cfg, t0);

        {
            let entry = reg.entries.get_mut(&key).expect("entry exists");
            entry.state = LifecycleState::Decay(DecayStep::Step2);
        }

        let t1 = t0 + Duration::from_secs(500);
        let outcome = reg.on_fsevent(key.clone(), t1);

        assert!(!outcome.refresh);
        assert!(outcome.transition.is_none());
        assert_eq!(
            reg.state(&key),
            Some(&LifecycleState::Decay(DecayStep::Step2))
        );
    }
}
