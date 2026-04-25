//! Cache entry lifecycle state machine.
//!
//! Executable form of the behaviour defined in `docs/cache-lifecycle.md`.
//! State transitions: Cold → Active → Decay1..4 → Evicted. Per-entry state
//! held in `LifecycleEntry`; scheduler dispatches on demand/fsevent/tick
//! via `LifecycleRegistry`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::provider::{InvalidationStrategy, KeepAlive};

/// Lifecycle key: (provider_name, path, source_name).
/// Path is None for Global sources; Some(canonical_path) for PathScoped.
pub type Key = (String, Option<String>, String);

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
pub enum StrategyKind {
    Poll,
    Watch,
    WatchAndPoll,
}

#[derive(Debug, Clone)]
pub struct SourceLifecycleConfig {
    pub strategy_kind: StrategyKind,
    /// Some for Poll/WatchAndPoll, None for pure Watch.
    pub poll_interval: Option<Duration>,
    pub keep_alive: KeepAlive,
    pub fsevents_reinstate: bool,
}

impl SourceLifecycleConfig {
    pub fn from_strategy(
        strategy: &InvalidationStrategy,
        keep_alive: KeepAlive,
        fsevents_reinstate: bool,
    ) -> Self {
        let (kind, poll_interval) = match strategy {
            InvalidationStrategy::Poll { interval_secs } => {
                (StrategyKind::Poll, Some(Duration::from_secs(*interval_secs)))
            }
            InvalidationStrategy::Watch { .. } => (StrategyKind::Watch, None),
            InvalidationStrategy::WatchAndPoll { interval_secs, .. } => {
                (StrategyKind::WatchAndPoll, Some(Duration::from_secs(*interval_secs)))
            }
        };
        Self {
            strategy_kind: kind,
            poll_interval,
            keep_alive,
            fsevents_reinstate,
        }
    }

    /// True for Watch + Global + KeepAlive::Never.
    pub fn never_decays(&self) -> bool {
        matches!(self.keep_alive, KeepAlive::Never)
    }
}


#[derive(Debug, Clone)]
pub struct LifecycleEntry {
    pub state: LifecycleState,
    /// None for pure Watch sources (no poll path).
    pub poll_timer: Option<PollTimer>,
    /// None for pure-watch globals (KeepAlive::Never).
    pub decay_timer: Option<DecayTimer>,
    pub config: SourceLifecycleConfig,
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
        config: SourceLifecycleConfig,
        now: Instant,
    ) -> DemandOutcome {
        if config.never_decays() {
            // Pure-watch global: enter Active, never decay. No poll timer.
            match self.entries.get_mut(&key) {
                None => {
                    self.entries.insert(
                        key,
                        LifecycleEntry {
                            state: LifecycleState::Active,
                            poll_timer: None,
                            decay_timer: None,
                            config,
                        },
                    );
                    return DemandOutcome {
                        transition: StateTransition::NewlyActive,
                        watch_registration: WatchAction::Register,
                    };
                }
                Some(_) => {
                    // Already Active. No keep-alive to reset.
                    return DemandOutcome {
                        transition: StateTransition::ResetKeepAlive,
                        watch_registration: WatchAction::Preserve,
                    };
                }
            }
        }

        let keep_alive_duration = match (&config.keep_alive, &config.poll_interval) {
            (KeepAlive::Polls(k), Some(p)) => *p * *k,
            (KeepAlive::Duration(secs), _) => Duration::from_secs(*secs),
            (KeepAlive::Polls(_), None) => {
                // Misconfigured — should have been caught at registration. Defensive.
                return DemandOutcome {
                    transition: StateTransition::ResetKeepAlive,
                    watch_registration: WatchAction::Preserve,
                };
            }
            (KeepAlive::Never, _) => unreachable!("never_decays() handled above"),
        };

        match self.entries.get_mut(&key) {
            None => {
                // Cold → Active
                let poll_timer = config
                    .poll_interval
                    .map(|p| PollTimer { last_fired: now, interval: p });
                self.entries.insert(
                    key,
                    LifecycleEntry {
                        state: LifecycleState::Active,
                        poll_timer,
                        decay_timer: Some(DecayTimer {
                            last_demand: now,
                            step_deadline: now + keep_alive_duration,
                        }),
                        config,
                    },
                );
                DemandOutcome {
                    transition: StateTransition::NewlyActive,
                    watch_registration: WatchAction::Register,
                }
            }
            Some(entry) => match entry.state {
                LifecycleState::Active => {
                    if let Some(d) = entry.decay_timer.as_mut() {
                        d.last_demand = now;
                        d.step_deadline = now + keep_alive_duration;
                    }
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
                    if let Some(pt) = entry.poll_timer.as_mut() {
                        pt.last_fired = now;
                        if let Some(pi) = config.poll_interval {
                            pt.interval = pi;
                        }
                    }
                    if let Some(d) = entry.decay_timer.as_mut() {
                        d.last_demand = now;
                        d.step_deadline = now + keep_alive_duration;
                    }
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

    pub fn tick(&mut self, now: Instant) -> TickActions {
        let mut actions = TickActions::default();

        for (key, entry) in self.entries.iter_mut() {
            // Poll timer fires only when present.
            if let Some(pt) = entry.poll_timer.as_mut() {
                let next_due = pt.last_fired + pt.interval;
                if now >= next_due {
                    actions.polls_due.push(key.clone());
                    pt.last_fired = now;
                }
            }

            // Decay timer fires only when present.
            let Some(dt) = entry.decay_timer.as_mut() else {
                continue;
            };
            if now < dt.step_deadline {
                continue;
            }

            let next_state: Option<LifecycleState> = match entry.state {
                LifecycleState::Active => Some(LifecycleState::Decay(DecayStep::Step1)),
                LifecycleState::Decay(step) => step.next().map(LifecycleState::Decay),
            };

            match next_state {
                Some(new_state) => {
                    entry.state = new_state;
                    if let LifecycleState::Decay(step) = new_state {
                        let rate_mult = step.rate_multiplier();

                        // Step duration depends on keep-alive variant.
                        let step_duration = match (&entry.config.keep_alive, &entry.config.poll_interval) {
                            (KeepAlive::Polls(k), Some(p)) => *p * rate_mult * *k,
                            (KeepAlive::Duration(secs), _) => Duration::from_secs(*secs) * rate_mult,
                            _ => continue,
                        };
                        dt.step_deadline = now + step_duration;

                        // Adjust poll interval if there is a poll path.
                        if let Some(pt) = entry.poll_timer.as_mut() {
                            if let Some(p) = entry.config.poll_interval {
                                pt.interval = p * rate_mult;
                            }
                        }

                        // Drop watches on Active→Decay1 if !fsevents_reinstate.
                        if step == DecayStep::Step1 && !entry.config.fsevents_reinstate {
                            actions.watch_drops.push(key.clone());
                        }
                    }
                    actions.transitions.push((key.clone(), new_state));
                }
                None => {
                    // Decay4 → Evicted.
                    actions.evictions.push(key.clone());
                }
            }
        }

        // Remove evicted entries after iteration.
        for key in &actions.evictions {
            self.entries.remove(key);
        }

        actions
    }

    pub fn poll_interval(&self, key: &Key) -> Option<Duration> {
        self.entries.get(key).and_then(|e| e.poll_timer.as_ref().map(|pt| pt.interval))
    }

    pub fn state(&self, key: &Key) -> Option<&LifecycleState> {
        self.entries.get(key).map(|e| &e.state)
    }

    pub fn remove(&mut self, key: &Key) {
        self.entries.remove(key);
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Key, &LifecycleEntry)> {
        self.entries.iter()
    }
}

/// Convert a `LifecycleState` to a numeric decay level: 0 = Active, 1–4 = Decay steps.
/// Used by the scheduler and status renderer to populate `CacheRow::decay`.
pub fn to_decay_level(state: &LifecycleState) -> u8 {
    match state {
        LifecycleState::Active => 0,
        LifecycleState::Decay(step) => step.as_u8(),
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
        (provider.to_string(), Some(path.to_string()), "main".to_string())
    }

    fn test_key_global(provider: &str) -> Key {
        (provider.to_string(), None, "main".to_string())
    }

    fn test_config() -> SourceLifecycleConfig {
        SourceLifecycleConfig {
            strategy_kind: StrategyKind::Poll,
            poll_interval: Some(Duration::from_secs(60)),
            keep_alive: KeepAlive::Polls(12),
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
        assert_eq!(entry.decay_timer.as_ref().unwrap().last_demand, t1);
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
            entry.poll_timer.as_mut().unwrap().interval = Duration::from_secs(240);
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
        assert_eq!(entry.decay_timer.as_ref().unwrap().last_demand, t1);
    }

    /// Reinstatement when fsevents_reinstate = true: watches were preserved,
    /// so watch_registration is Preserve (not Reinstate).
    #[test]
    fn on_demand_reinstatement_preserves_watches_when_fsevents_reinstate() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key("mise", "/repo");
        let t0 = Instant::now();
        let cfg = SourceLifecycleConfig {
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
        assert_eq!(entry.decay_timer.as_ref().unwrap().last_demand, t1);
    }

    /// fsevent while decaying with fsevents_reinstate = true: reinstates to Active.
    #[test]
    fn on_fsevent_on_decay_with_fsevents_reinstate_true_reinstates() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key("mise", "/repo");
        let t0 = Instant::now();
        let cfg = SourceLifecycleConfig {
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

    /// Scenario: Polling refreshes an active entry.
    #[test]
    fn tick_fires_poll_when_interval_elapsed_in_active() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key("git", "/repo");
        let t0 = Instant::now();

        reg.on_demand(key.clone(), test_config(), t0);

        // Poll interval is 60s. Tick at 61s — should fire.
        let t1 = t0 + Duration::from_secs(61);
        let actions = reg.tick(t1);

        assert!(actions.polls_due.contains(&key), "poll should fire");
        let entry = reg.entries.get(&key).expect("entry exists");
        assert_eq!(entry.poll_timer.as_ref().unwrap().last_fired, t1);
    }

    #[test]
    fn tick_does_not_fire_poll_before_interval() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key("git", "/repo");
        let t0 = Instant::now();

        reg.on_demand(key.clone(), test_config(), t0);

        let t1 = t0 + Duration::from_secs(30);
        let actions = reg.tick(t1);

        assert!(actions.polls_due.is_empty());
        let entry = reg.entries.get(&key).expect("entry exists");
        assert_eq!(entry.poll_timer.as_ref().unwrap().last_fired, t0);
    }

    /// Scenario: Poll timer and decay timer advance independently.
    #[test]
    fn tick_poll_fire_does_not_reset_decay_timer() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key("git", "/repo");
        let t0 = Instant::now();

        reg.on_demand(key.clone(), test_config(), t0);

        // Poll fires at 61s; decay timer should still register last_demand at t0.
        let t1 = t0 + Duration::from_secs(61);
        reg.tick(t1);

        let entry = reg.entries.get(&key).expect("entry exists");
        assert_eq!(
            entry.decay_timer.as_ref().unwrap().last_demand,
            t0,
            "decay timer should not reset on poll fire"
        );
    }

    /// Scenario: Keep-alive expiry enters Decay1.
    #[test]
    fn tick_advances_active_to_decay1_when_keep_alive_elapses() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key("git", "/repo");
        let t0 = Instant::now();

        reg.on_demand(key.clone(), test_config(), t0);

        // Keep-alive = K*P = 12*60 = 720s.
        let t1 = t0 + Duration::from_secs(721);
        let actions = reg.tick(t1);

        assert_eq!(
            reg.state(&key),
            Some(&LifecycleState::Decay(DecayStep::Step1))
        );
        assert_eq!(reg.poll_interval(&key), Some(Duration::from_secs(120)));
        assert!(
            actions.watch_drops.contains(&key),
            "drop-on-decay default: watches dropped on Decay1 entry"
        );
        assert!(
            actions
                .transitions
                .iter()
                .any(|(k, s)| k == &key && s == &LifecycleState::Decay(DecayStep::Step1))
        );
    }

    /// Active → Decay1 with fsevents_reinstate=true: watches NOT dropped.
    #[test]
    fn tick_advance_with_fsevents_reinstate_preserves_watches() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key("mise", "/repo");
        let t0 = Instant::now();
        let cfg = SourceLifecycleConfig {
            fsevents_reinstate: true,
            ..test_config()
        };

        reg.on_demand(key.clone(), cfg, t0);

        let t1 = t0 + Duration::from_secs(721);
        let actions = reg.tick(t1);

        assert_eq!(
            reg.state(&key),
            Some(&LifecycleState::Decay(DecayStep::Step1))
        );
        assert!(
            !actions.watch_drops.contains(&key),
            "watches should be preserved when fsevents_reinstate=true"
        );
    }

    /// Decay1 → Decay2 → Decay3 → Decay4. Each step doubles poll interval.
    #[test]
    fn tick_advances_through_all_decay_steps() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key("git", "/repo");
        let t0 = Instant::now();

        reg.on_demand(key.clone(), test_config(), t0);

        // Enter Decay1 at keep-alive = 720s.
        let t_decay1 = t0 + Duration::from_secs(721);
        reg.tick(t_decay1);

        // Step1 duration = K * 2P = 12 * 120 = 1440s.
        // Decay1 step_deadline = t_decay1 + 1440. Tick past it.
        let t_decay2 = t_decay1 + Duration::from_secs(1441);
        let actions = reg.tick(t_decay2);
        assert_eq!(
            reg.state(&key),
            Some(&LifecycleState::Decay(DecayStep::Step2))
        );
        assert_eq!(reg.poll_interval(&key), Some(Duration::from_secs(240)));
        assert!(
            actions
                .transitions
                .iter()
                .any(|(_, s)| s == &LifecycleState::Decay(DecayStep::Step2))
        );

        // Step2 duration = K * 4P = 2880s.
        let t_decay3 = t_decay2 + Duration::from_secs(2881);
        reg.tick(t_decay3);
        assert_eq!(
            reg.state(&key),
            Some(&LifecycleState::Decay(DecayStep::Step3))
        );
        assert_eq!(reg.poll_interval(&key), Some(Duration::from_secs(480)));

        // Step3 duration = K * 8P = 5760s.
        let t_decay4 = t_decay3 + Duration::from_secs(5761);
        reg.tick(t_decay4);
        assert_eq!(
            reg.state(&key),
            Some(&LifecycleState::Decay(DecayStep::Step4))
        );
        assert_eq!(reg.poll_interval(&key), Some(Duration::from_secs(960)));
    }

    /// Every lifecycle step contains exactly K polls at its current rate.
    /// Verified by checking that step_deadline = previous_deadline + K*P*2^n.
    #[test]
    fn tick_every_step_contains_k_polls() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key("git", "/repo");
        let t0 = Instant::now();
        reg.on_demand(key.clone(), test_config(), t0);

        // Enter Decay1 just past keep-alive.
        let after_keep_alive = t0 + Duration::from_secs(721);
        reg.tick(after_keep_alive);

        // Decay1 has poll interval 2P = 120s and step_deadline should be
        // after_keep_alive + K*2P = after_keep_alive + 1440s.
        let entry = reg.entries.get(&key).expect("entry exists");
        let expected_deadline = after_keep_alive + Duration::from_secs(1440);
        let actual_deadline = entry.decay_timer.as_ref().unwrap().step_deadline;

        // Exact equality; step_deadline = now + K * P * rate_mult by construction.
        assert_eq!(
            actual_deadline, expected_deadline,
            "step_deadline should be exactly now + K*P*2 for Decay1"
        );
    }

    /// Scenario: Decay4 expiry evicts the entry.
    #[test]
    fn tick_evicts_entry_at_decay4_expiry() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key("git", "/repo");
        let t0 = Instant::now();

        reg.on_demand(key.clone(), test_config(), t0);

        // Walk through all 4 decay steps.
        // Keep-alive = 720s → Decay1 at 721.
        let t_decay1 = t0 + Duration::from_secs(721);
        reg.tick(t_decay1);

        // Decay1 step_deadline = t_decay1 + 1440. Advance to Decay2.
        let t_decay2 = t_decay1 + Duration::from_secs(1441);
        reg.tick(t_decay2);

        // Decay2 step_deadline = t_decay2 + 2880. Advance to Decay3.
        let t_decay3 = t_decay2 + Duration::from_secs(2881);
        reg.tick(t_decay3);

        // Decay3 step_deadline = t_decay3 + 5760. Advance to Decay4.
        let t_decay4 = t_decay3 + Duration::from_secs(5761);
        reg.tick(t_decay4);

        assert_eq!(
            reg.state(&key),
            Some(&LifecycleState::Decay(DecayStep::Step4))
        );

        // Decay4 step_deadline = t_decay4 + 11520. Advance past it.
        let t_evict = t_decay4 + Duration::from_secs(11521);
        let actions = reg.tick(t_evict);

        assert!(
            actions.evictions.contains(&key),
            "Decay4 expiry should evict; got actions {actions:?}"
        );
        assert!(reg.state(&key).is_none(), "entry should be removed");
    }

    /// Scenario: Total lifetime from last demand to eviction is 31KP seconds.
    /// Active K*P + Decay (2+4+8+16) * K * P = 31 * K * P.
    /// With K=12, P=60s: 31 * 12 * 60 = 22320s.
    #[test]
    fn total_lifetime_is_31_k_p_from_last_demand_to_eviction() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key("git", "/repo");
        let t0 = Instant::now();

        reg.on_demand(key.clone(), test_config(), t0);

        // Active boundary: 720. Decay1 at 721.
        reg.tick(t0 + Duration::from_secs(721));

        // Decay1 boundary (t=721 + 1440 = 2161).
        reg.tick(t0 + Duration::from_secs(2161));

        // Decay2 boundary (t=2161 + 2880 = 5041).
        reg.tick(t0 + Duration::from_secs(5041));

        // Decay3 boundary (t=5041 + 5760 = 10801).
        reg.tick(t0 + Duration::from_secs(10801));

        // Decay4 boundary (t=10801 + 11520 = 22321).
        reg.tick(t0 + Duration::from_secs(22321));

        assert!(
            reg.state(&key).is_none(),
            "entry should be evicted after 31KP seconds"
        );
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

    /// Watch source with KeepAlive::Duration advances through decay steps.
    #[test]
    fn tick_advances_watch_duration_through_decay_steps() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key("git", "/repo");
        let cfg = SourceLifecycleConfig {
            strategy_kind: StrategyKind::Watch,
            poll_interval: None,
            keep_alive: KeepAlive::Duration(60),
            fsevents_reinstate: false,
        };
        let t0 = Instant::now();
        reg.on_demand(key.clone(), cfg, t0);

        // After 60s, should enter Decay1
        let actions = reg.tick(t0 + Duration::from_secs(60));
        assert!(
            actions
                .transitions
                .iter()
                .any(|(_, s)| matches!(s, LifecycleState::Decay(DecayStep::Step1)))
        );
    }

    /// Pure-watch global (KeepAlive::Never) stays Active indefinitely.
    #[test]
    fn never_decays_stays_active_forever() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key_global("hostname");
        let cfg = SourceLifecycleConfig {
            strategy_kind: StrategyKind::Watch,
            poll_interval: None,
            keep_alive: KeepAlive::Never,
            fsevents_reinstate: false,
        };
        let t0 = Instant::now();
        reg.on_demand(key.clone(), cfg, t0);

        let actions = reg.tick(t0 + Duration::from_secs(86400));
        assert!(actions.transitions.is_empty());
        assert!(actions.evictions.is_empty());
        assert_eq!(reg.state(&key), Some(&LifecycleState::Active));
    }
}
