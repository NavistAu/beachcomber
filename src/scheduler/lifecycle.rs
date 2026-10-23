//! Cache entry lifecycle state machine.
//!
//! Executable form of the behaviour described in `docs/architecture.md` §5.
//! State transitions: Cold → Active → Decay(1..=depth) → Evicted (depth defaults to 4). Per-entry state
//! held in `LifecycleEntry`; scheduler dispatches on demand/fsevent/tick
//! via `LifecycleRegistry`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::provider::InvalidationStrategy;

/// Decay steps before eviction unless a lifecycle says otherwise.
pub const DEFAULT_DECAY: u8 = 4;

/// Poll interval a watch-only Source's Active window is measured in.
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 60;

/// Polls in the Active window unless a strategy says otherwise.
pub const DEFAULT_POLL_COUNT: u32 = 12;

/// Failed executions before a Source backs off, unless it says otherwise.
pub const DEFAULT_FAILBACK_COUNT: u32 = 3;

/// Seconds between a failing Source's reattempts, unless it says otherwise.
pub const DEFAULT_FAILBACK_INTERVAL_SECS: u64 = 1;

/// How long one `execute` may take before the engine reports it slow.
pub const DEFAULT_BUDGET_EXECUTE: Duration = Duration::from_millis(50);

/// Lifecycle key. `path` is None for Global sources; Some(canonical_path) for
/// PathScoped. `node` is the put address a lifecycle belongs to (`source` is
/// `"put"` then) and `""` for every Source instance.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key {
    pub provider: String,
    pub path: Option<String>,
    pub source: String,
    pub node: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleState {
    Active,
    /// 1 through the config's `decay`; step n lasts `lifespan × 2^n`.
    Decay(u8),
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
    /// Polls in the Active window; the window is `interval × count`.
    pub poll_count: u32,
    pub fsevents_reinstate: bool,
    /// Decay steps before eviction, at least 1.
    pub decay: u8,
}

impl SourceLifecycleConfig {
    pub fn from_strategy(
        strategy: &InvalidationStrategy,
        fsevents_reinstate: bool,
        decay: u8,
    ) -> Self {
        let (kind, poll_interval, poll_count) = match strategy {
            InvalidationStrategy::Poll {
                interval_secs,
                count,
            } => (
                StrategyKind::Poll,
                Some(Duration::from_secs(*interval_secs)),
                *count,
            ),
            InvalidationStrategy::Watch { .. } => (StrategyKind::Watch, None, DEFAULT_POLL_COUNT),
            InvalidationStrategy::WatchAndPoll {
                interval_secs,
                count,
                ..
            } => (
                StrategyKind::WatchAndPoll,
                Some(Duration::from_secs(*interval_secs)),
                *count,
            ),
        };
        Self {
            strategy_kind: kind,
            poll_interval,
            poll_count,
            fsevents_reinstate,
            decay,
        }
    }

    /// The Active window: `interval × count`; a watch-only Source has no
    /// interval of its own and takes the default one.
    pub fn lifespan(&self) -> Duration {
        self.poll_interval
            .unwrap_or(Duration::from_secs(DEFAULT_POLL_INTERVAL_SECS))
            * self.poll_count
    }
}

#[derive(Debug, Clone)]
pub struct LifecycleEntry {
    pub state: LifecycleState,
    /// None for pure Watch sources (no poll path).
    pub poll_timer: Option<PollTimer>,
    pub decay_timer: DecayTimer,
    pub config: SourceLifecycleConfig,
}

pub struct LifecycleRegistry {
    entries: HashMap<Key, LifecycleEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateTransition {
    NewlyActive,
    ResetActive,
    Reinstated { from: u8 },
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
        debug_assert!(config.decay >= 1, "decay depth must be at least 1");
        let lifespan = config.lifespan();

        match self.entries.get_mut(&key) {
            None => {
                // Cold → Active
                let poll_timer = config.poll_interval.map(|p| PollTimer {
                    last_fired: now,
                    interval: p,
                });
                self.entries.insert(
                    key,
                    LifecycleEntry {
                        state: LifecycleState::Active,
                        poll_timer,
                        decay_timer: DecayTimer {
                            last_demand: now,
                            step_deadline: now + lifespan,
                        },
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
                    entry.decay_timer.last_demand = now;
                    entry.decay_timer.step_deadline = now + lifespan;
                    entry.config = config;
                    DemandOutcome {
                        transition: StateTransition::ResetActive,
                        watch_registration: WatchAction::Preserve,
                    }
                }
                LifecycleState::Decay(from) => {
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
                    entry.decay_timer.last_demand = now;
                    entry.decay_timer.step_deadline = now + lifespan;
                    entry.config = config;
                    DemandOutcome {
                        transition: StateTransition::Reinstated { from },
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
            let dt = &mut entry.decay_timer;
            if now < dt.step_deadline {
                continue;
            }

            let next_state: Option<LifecycleState> = match entry.state {
                LifecycleState::Active => Some(LifecycleState::Decay(1)),
                LifecycleState::Decay(n) => {
                    (n < entry.config.decay).then_some(LifecycleState::Decay(n + 1))
                }
            };

            match next_state {
                Some(new_state) => {
                    entry.state = new_state;
                    if let LifecycleState::Decay(step) = new_state {
                        let rate_mult = 1u32 << step;

                        dt.step_deadline = now + entry.config.lifespan() * rate_mult;

                        // Adjust poll interval if there is a poll path.
                        if let Some(pt) = entry.poll_timer.as_mut()
                            && let Some(p) = entry.config.poll_interval
                        {
                            pt.interval = p * rate_mult;
                        }

                        // Drop watches on Active→Decay1 if !fsevents_reinstate.
                        if step == 1 && !entry.config.fsevents_reinstate {
                            actions.watch_drops.push(key.clone());
                        }
                    }
                    actions.transitions.push((key.clone(), new_state));
                }
                None => {
                    // Last decay step → Evicted.
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
        self.entries
            .get(key)
            .and_then(|e| e.poll_timer.as_ref().map(|pt| pt.interval))
    }

    pub fn state(&self, key: &Key) -> Option<&LifecycleState> {
        self.entries.get(key).map(|e| &e.state)
    }

    pub fn remove(&mut self, key: &Key) {
        self.entries.remove(key);
    }

    /// Remove every `put` lifecycle key for `(provider, path)` at `node` or
    /// below it (`""` is the namespace root: every one).
    pub fn remove_put_keys(&mut self, provider: &str, path: Option<&str>, node: &str) {
        self.entries.retain(|key, _| {
            !(key.source == crate::cache::PUT_SOURCE
                && key.provider == provider
                && key.path.as_deref() == path
                && crate::cache::is_at_or_below(&key.node, node))
        });
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Key, &LifecycleEntry)> {
        self.entries.iter()
    }
}

/// Convert a `LifecycleState` to a numeric decay level: 0 = Active, n = Decay(n).
/// Used by the scheduler and status renderer to populate `CacheRow::decay`.
pub fn to_decay_level(state: &LifecycleState) -> u8 {
    match state {
        LifecycleState::Active => 0,
        LifecycleState::Decay(n) => *n,
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
        Key {
            provider: provider.to_string(),
            path: Some(path.to_string()),
            source: "main".to_string(),
            node: String::new(),
        }
    }

    fn test_key_global(provider: &str) -> Key {
        Key {
            provider: provider.to_string(),
            path: None,
            source: "main".to_string(),
            node: String::new(),
        }
    }

    fn test_config() -> SourceLifecycleConfig {
        SourceLifecycleConfig {
            strategy_kind: StrategyKind::Poll,
            poll_interval: Some(Duration::from_secs(60)),
            poll_count: 12,
            fsevents_reinstate: false,
            decay: DEFAULT_DECAY,
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

    /// Scenario: Warm read resets Active window.
    #[test]
    fn on_demand_on_active_resets_the_window() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key("git", "/repo");
        let t0 = Instant::now();
        let cfg = test_config();

        reg.on_demand(key.clone(), cfg.clone(), t0);
        let t1 = t0 + Duration::from_secs(100);

        let outcome = reg.on_demand(key.clone(), cfg, t1);

        assert_eq!(outcome.transition, StateTransition::ResetActive);
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
            entry.state = LifecycleState::Decay(2);
            entry.poll_timer.as_mut().unwrap().interval = Duration::from_secs(240);
        }

        let t1 = t0 + Duration::from_secs(500);
        let outcome = reg.on_demand(key.clone(), cfg, t1);

        assert_eq!(outcome.transition, StateTransition::Reinstated { from: 2 });
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
        let cfg = SourceLifecycleConfig {
            fsevents_reinstate: true,
            ..test_config()
        };

        reg.on_demand(key.clone(), cfg.clone(), t0);

        {
            let entry = reg.entries.get_mut(&key).expect("entry exists");
            entry.state = LifecycleState::Decay(3);
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
            Some(StateTransition::ResetActive)
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
        let cfg = SourceLifecycleConfig {
            fsevents_reinstate: true,
            ..test_config()
        };

        reg.on_demand(key.clone(), cfg, t0);

        {
            let entry = reg.entries.get_mut(&key).expect("entry exists");
            entry.state = LifecycleState::Decay(3);
        }

        let t1 = t0 + Duration::from_secs(1000);
        let outcome = reg.on_fsevent(key.clone(), t1);

        assert!(outcome.refresh);
        assert!(matches!(
            outcome.transition,
            Some(StateTransition::Reinstated { from: 3 })
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
            entry.decay_timer.last_demand, t0,
            "decay timer should not reset on poll fire"
        );
    }

    /// Scenario: Active window expiry enters Decay1.
    #[test]
    fn tick_advances_active_to_decay1_when_the_window_elapses() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key("git", "/repo");
        let t0 = Instant::now();

        reg.on_demand(key.clone(), test_config(), t0);

        // Active window = K*P = 12*60 = 720s.
        let t1 = t0 + Duration::from_secs(721);
        let actions = reg.tick(t1);

        assert_eq!(reg.state(&key), Some(&LifecycleState::Decay(1)));
        assert_eq!(reg.poll_interval(&key), Some(Duration::from_secs(120)));
        assert!(
            actions.watch_drops.contains(&key),
            "drop-on-decay default: watches dropped on Decay1 entry"
        );
        assert!(
            actions
                .transitions
                .iter()
                .any(|(k, s)| k == &key && s == &LifecycleState::Decay(1))
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

        assert_eq!(reg.state(&key), Some(&LifecycleState::Decay(1)));
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

        // Enter Decay1 at Active window = 720s.
        let t_decay1 = t0 + Duration::from_secs(721);
        reg.tick(t_decay1);

        // Step1 duration = K * 2P = 12 * 120 = 1440s.
        // Decay1 step_deadline = t_decay1 + 1440. Tick past it.
        let t_decay2 = t_decay1 + Duration::from_secs(1441);
        let actions = reg.tick(t_decay2);
        assert_eq!(reg.state(&key), Some(&LifecycleState::Decay(2)));
        assert_eq!(reg.poll_interval(&key), Some(Duration::from_secs(240)));
        assert!(
            actions
                .transitions
                .iter()
                .any(|(_, s)| s == &LifecycleState::Decay(2))
        );

        // Step2 duration = K * 4P = 2880s.
        let t_decay3 = t_decay2 + Duration::from_secs(2881);
        reg.tick(t_decay3);
        assert_eq!(reg.state(&key), Some(&LifecycleState::Decay(3)));
        assert_eq!(reg.poll_interval(&key), Some(Duration::from_secs(480)));

        // Step3 duration = K * 8P = 5760s.
        let t_decay4 = t_decay3 + Duration::from_secs(5761);
        reg.tick(t_decay4);
        assert_eq!(reg.state(&key), Some(&LifecycleState::Decay(4)));
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

        // Enter Decay1 just past Active window.
        let after_window = t0 + Duration::from_secs(721);
        reg.tick(after_window);

        // Decay1 has poll interval 2P = 120s and step_deadline should be
        // after_window + K*2P = after_window + 1440s.
        let entry = reg.entries.get(&key).expect("entry exists");
        let expected_deadline = after_window + Duration::from_secs(1440);
        let actual_deadline = entry.decay_timer.step_deadline;

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
        // Active window = 720s → Decay1 at 721.
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

        assert_eq!(reg.state(&key), Some(&LifecycleState::Decay(4)));

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
            entry.state = LifecycleState::Decay(2);
        }

        let t1 = t0 + Duration::from_secs(500);
        let outcome = reg.on_fsevent(key.clone(), t1);

        assert!(!outcome.refresh);
        assert!(outcome.transition.is_none());
        assert_eq!(reg.state(&key), Some(&LifecycleState::Decay(2)));
    }

    fn watch_only() -> SourceLifecycleConfig {
        SourceLifecycleConfig::from_strategy(
            &InvalidationStrategy::Watch {
                patterns: vec![],
                abs_paths: vec![],
            },
            false,
            DEFAULT_DECAY,
        )
    }

    /// A watch-only Source has no poll of its own and takes 60 s x 12.
    #[test]
    fn a_watch_only_source_stays_active_for_twelve_minutes() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key("git", "/repo");
        let t0 = Instant::now();
        reg.on_demand(key.clone(), watch_only(), t0);

        reg.tick(t0 + Duration::from_secs(719));
        assert_eq!(reg.state(&key), Some(&LifecycleState::Active));

        let actions = reg.tick(t0 + Duration::from_secs(720));
        assert!(
            actions
                .transitions
                .iter()
                .any(|(_, s)| matches!(s, LifecycleState::Decay(1)))
        );
    }

    /// Nothing is exempt from decay: a global watch Source decays 12 minutes
    /// after its last read and is evicted at the end of its ladder.
    #[test]
    fn a_global_watch_source_now_decays() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key_global("hostname");
        let t0 = Instant::now();
        reg.on_demand(key.clone(), watch_only(), t0);

        reg.tick(t0 + Duration::from_secs(720));
        assert_eq!(reg.state(&key), Some(&LifecycleState::Decay(1)));

        // Active 720 s, then Decay(n) lasts 720 x 2^n: 1440, 2880, 5760, 11520.
        let mut t = 720;
        for step in 1..=4u64 {
            t += 720 * (1 << step);
            reg.tick(t0 + Duration::from_secs(t));
        }
        assert_eq!(reg.state(&key), None);
    }

    /// The Active window is the poll interval times the count.
    #[test]
    fn the_active_window_is_interval_times_count() {
        let mut cfg = ten_second_poll(DEFAULT_DECAY);
        cfg.poll_count = 3;
        assert_eq!(cfg.lifespan(), Duration::from_secs(30));

        let mut reg = LifecycleRegistry::new();
        let key = test_key_global("p");
        let t0 = Instant::now();
        reg.on_demand(key.clone(), cfg, t0);
        reg.tick(t0 + Duration::from_secs(29));
        assert_eq!(reg.state(&key), Some(&LifecycleState::Active));
        reg.tick(t0 + Duration::from_secs(30));
        assert_eq!(reg.state(&key), Some(&LifecycleState::Decay(1)));
    }

    fn ten_second_poll(decay: u8) -> SourceLifecycleConfig {
        SourceLifecycleConfig {
            strategy_kind: StrategyKind::Poll,
            poll_interval: Some(Duration::from_secs(10)),
            poll_count: 1,
            fsevents_reinstate: false,
            decay,
        }
    }

    /// Depth 1: valid for one poll, stale for one doubled step, then gone.
    #[test]
    fn depth_one_is_valid_then_stale_for_one_step_then_evicted() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key_global("p");
        let t0 = Instant::now();
        reg.on_demand(key.clone(), ten_second_poll(1), t0);

        reg.tick(t0 + Duration::from_secs(9));
        assert_eq!(reg.state(&key), Some(&LifecycleState::Active));

        reg.tick(t0 + Duration::from_secs(10));
        assert_eq!(reg.state(&key), Some(&LifecycleState::Decay(1)));

        reg.tick(t0 + Duration::from_secs(29));
        assert_eq!(reg.state(&key), Some(&LifecycleState::Decay(1)));

        let actions = reg.tick(t0 + Duration::from_secs(30));
        assert_eq!(actions.evictions, vec![key.clone()]);
        assert_eq!(reg.state(&key), None);
    }

    /// Depth 2: Decay(1), Decay(2), then evicted.
    #[test]
    fn depth_two_evicts_after_two_steps() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key_global("p");
        let t0 = Instant::now();
        reg.on_demand(key.clone(), ten_second_poll(2), t0);

        reg.tick(t0 + Duration::from_secs(10));
        assert_eq!(reg.state(&key), Some(&LifecycleState::Decay(1)));
        // Step 1 lasts 10 * 2 = 20 s.
        reg.tick(t0 + Duration::from_secs(30));
        assert_eq!(reg.state(&key), Some(&LifecycleState::Decay(2)));
        // Step 2 lasts 10 * 4 = 40 s.
        reg.tick(t0 + Duration::from_secs(69));
        assert_eq!(reg.state(&key), Some(&LifecycleState::Decay(2)));
        let actions = reg.tick(t0 + Duration::from_secs(70));
        assert_eq!(actions.evictions, vec![key.clone()]);
        assert_eq!(reg.state(&key), None);
    }

    /// Depth 4 is the standard ladder: Active, Decay(1..=4), evicted at 10 * 31 s.
    #[test]
    fn depth_four_walks_the_standard_ladder() {
        let mut reg = LifecycleRegistry::new();
        let key = test_key_global("p");
        let t0 = Instant::now();
        reg.on_demand(key.clone(), ten_second_poll(DEFAULT_DECAY), t0);

        let mut t = 10;
        for n in 1..=4u8 {
            reg.tick(t0 + Duration::from_secs(t));
            assert_eq!(reg.state(&key), Some(&LifecycleState::Decay(n)));
            assert_eq!(to_decay_level(&LifecycleState::Decay(n)), n);
            // Step n lasts 10 * 2^n s.
            t += 10 * (1 << n);
        }
        let actions = reg.tick(t0 + Duration::from_secs(t));
        assert_eq!(actions.evictions, vec![key.clone()]);
        assert_eq!(t, 310);
    }
}
