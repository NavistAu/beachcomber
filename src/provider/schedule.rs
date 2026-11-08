//! A Source's schedule: what its `return` chain says of when it runs, how its
//! failures back off, and how long a read and a run may wait.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// `poll(interval, count)`: polls in the Active window, which lasts
/// `interval × count`. A count of 0 is a `ttl` (only it writes one): live for
/// `interval`, never polled, then one decay step as long.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Poll {
    pub interval_secs: u64,
    pub count: u32,
}

/// `failback(interval, reattempts)`: failed runs before the Source backs off,
/// and the time between its reattempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailbackConfig {
    pub reattempts: u32,
    pub interval_secs: u64,
}

/// Everything a Source's `return` chain records about its schedule, with the
/// engine's defaults filled in. The scheduler's lifecycle, the run's failure
/// backoff and the `<p>:cache` facet all read this one value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    /// `poll(...)`, or a `ttl(...)` as a poll of count 0. `None` with no
    /// watch either is a Source that runs only when read.
    pub poll: Option<Poll>,
    /// `watch(...)` paths relative to the instance's root, as written.
    pub watch: Vec<String>,
    /// `watch(...)` paths that are absolute, `~` and `$XDG_*` expanded.
    pub watch_abs: Vec<String>,
    /// Decay steps before eviction, when the Source sets its own (`ttl(...)`
    /// sets 1). `None` takes the provider's `_.decay` prototype.
    pub decay: Option<u8>,
    pub failback: FailbackConfig,
    /// How long a read of a stale record waits for its run before answering
    /// with the record: the Source's `budget(...)`, else `[daemon] budget`.
    pub budget: Duration,
    /// How long one `execute` may take before it is cut off: the Source's
    /// `timeout(...)`, else `[daemon] timeout`.
    pub timeout: Duration,
    /// How the Source's instances combine. `false` cascades: a read takes
    /// each leaf from the nearest instance that has it, then the next one up,
    /// ending at `~`. `true` is `| nearest`: the nearest instance answers
    /// alone, and no instance found is no value. Meaningless for a Source
    /// with no root, whose one instance is at `~` either way.
    pub nearest: bool,
}

impl Default for Schedule {
    /// No poll and no watch, at the built-in failback, budget and timeout.
    fn default() -> Self {
        use crate::scheduler::lifecycle::{
            DEFAULT_BUDGET, DEFAULT_FAILBACK_COUNT, DEFAULT_FAILBACK_INTERVAL_SECS, DEFAULT_TIMEOUT,
        };
        Self {
            poll: None,
            watch: Vec::new(),
            watch_abs: Vec::new(),
            decay: None,
            failback: FailbackConfig {
                reattempts: DEFAULT_FAILBACK_COUNT,
                interval_secs: DEFAULT_FAILBACK_INTERVAL_SECS,
            },
            budget: DEFAULT_BUDGET,
            timeout: DEFAULT_TIMEOUT,
            nearest: false,
        }
    }
}

impl Schedule {
    /// `poll(interval_secs, count)` and nothing else set.
    pub fn poll(interval_secs: u64, count: u32) -> Self {
        Self {
            poll: Some(Poll {
                interval_secs,
                count,
            }),
            ..Self::default()
        }
    }

    /// `watch(...)` and nothing else set.
    pub fn watch(watch: Vec<String>, watch_abs: Vec<String>) -> Self {
        Self {
            watch,
            watch_abs,
            ..Self::default()
        }
    }

    /// Whether it names any path to watch.
    pub fn watches(&self) -> bool {
        !self.watch.is_empty() || !self.watch_abs.is_empty()
    }

    /// Whether an instance on this schedule is invalidated by file events:
    /// anything but a poll with no watch beside it.
    pub fn watches_files(&self) -> bool {
        self.poll.is_none() || self.watches()
    }

    /// The ttl: a poll of count 0.
    pub fn ttl(&self) -> Option<Duration> {
        self.poll
            .filter(|p| p.count == 0)
            .map(|p| Duration::from_secs(p.interval_secs))
    }

    /// The interval a poll timer fires at: none for a ttl, which nothing
    /// polls.
    pub fn poll_interval(&self) -> Option<Duration> {
        self.poll
            .filter(|p| p.count > 0)
            .map(|p| Duration::from_secs(p.interval_secs))
    }

    /// The interval a record's staleness is measured against: the poll's,
    /// a ttl's included.
    pub fn expected_interval_secs(&self) -> Option<u64> {
        self.poll.map(|p| p.interval_secs)
    }

    /// What invalidates it: its poll and its watches.
    pub fn invalidation(&self) -> (Option<Poll>, &[String], &[String]) {
        (self.poll, &self.watch, &self.watch_abs)
    }
}
