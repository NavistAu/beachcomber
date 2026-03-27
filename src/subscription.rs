use crate::scheduler::TriggerSet;
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub enum BackoffStage {
    Grace,
    SlowPoll,
    Frozen,
    Evict,
}

#[derive(Debug)]
pub struct BackoffState {
    stage: BackoffStage,
    started_at: Instant,
    grace_duration: Duration,
}

impl BackoffState {
    pub fn new(grace_duration: Duration) -> Self {
        Self {
            stage: BackoffStage::Grace,
            started_at: Instant::now(),
            grace_duration,
        }
    }

    pub fn stage(&self) -> &BackoffStage {
        &self.stage
    }

    pub fn advance(&mut self) {
        self.stage = match self.stage {
            BackoffStage::Grace => BackoffStage::SlowPoll,
            BackoffStage::SlowPoll => BackoffStage::Frozen,
            BackoffStage::Frozen => BackoffStage::Evict,
            BackoffStage::Evict => BackoffStage::Evict,
        };
        self.started_at = Instant::now();
    }

    pub fn reset(&mut self, grace_duration: Duration) {
        self.stage = BackoffStage::Grace;
        self.started_at = Instant::now();
        self.grace_duration = grace_duration;
    }

    pub fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }

    pub fn grace_expired(&self) -> bool {
        matches!(self.stage, BackoffStage::Grace) && self.started_at.elapsed() >= self.grace_duration
    }

    pub fn poll_multiplier(&self) -> u64 {
        match self.stage {
            BackoffStage::Grace => 1,
            BackoffStage::SlowPoll => 4,
            BackoffStage::Frozen => 0,
            BackoffStage::Evict => 0,
        }
    }

    pub fn should_watch(&self) -> bool {
        matches!(self.stage, BackoffStage::Grace)
    }
}

type SubKey = (String, Option<String>);

#[derive(Debug, Clone)]
struct ConsumerSub {
    consumer_id: u64,
    triggers: TriggerSet,
}

pub struct SubscriptionManager {
    subs: HashMap<SubKey, Vec<ConsumerSub>>,
}

impl SubscriptionManager {
    pub fn new() -> Self {
        Self { subs: HashMap::new() }
    }

    pub fn subscribe(&mut self, consumer_id: u64, provider: &str, path: Option<&str>, triggers: TriggerSet) {
        let key = make_key(provider, path);
        let entry = self.subs.entry(key).or_default();
        if let Some(existing) = entry.iter_mut().find(|s| s.consumer_id == consumer_id) {
            existing.triggers = triggers;
        } else {
            entry.push(ConsumerSub { consumer_id, triggers });
        }
    }

    pub fn unsubscribe(&mut self, consumer_id: u64, provider: &str, path: Option<&str>) {
        let key = make_key(provider, path);
        if let Some(entry) = self.subs.get_mut(&key) {
            entry.retain(|s| s.consumer_id != consumer_id);
        }
    }

    pub fn disconnect(&mut self, consumer_id: u64) {
        for entry in self.subs.values_mut() {
            entry.retain(|s| s.consumer_id != consumer_id);
        }
    }

    pub fn subscriber_count(&self, provider: &str, path: Option<&str>) -> usize {
        let key = make_key(provider, path);
        self.subs.get(&key).map_or(0, |v| v.len())
    }

    pub fn effective_triggers(&self, provider: &str, path: Option<&str>) -> Option<TriggerSet> {
        self.effective_triggers_with_floor(provider, path, 0)
    }

    pub fn effective_triggers_with_floor(&self, provider: &str, path: Option<&str>, floor_secs: u64) -> Option<TriggerSet> {
        let key = make_key(provider, path);
        let subs = self.subs.get(&key)?;
        if subs.is_empty() { return None; }

        let mut watch = false;
        let mut min_poll: Option<u64> = None;

        for sub in subs {
            if sub.triggers.watch { watch = true; }
            if let Some(poll) = sub.triggers.poll_secs {
                min_poll = Some(match min_poll {
                    Some(current) => current.min(poll),
                    None => poll,
                });
            }
        }

        if let Some(ref mut poll) = min_poll {
            if *poll < floor_secs { *poll = floor_secs; }
        }

        Some(TriggerSet { watch, poll_secs: min_poll })
    }

    pub fn all_keys(&self) -> Vec<(String, Option<String>)> {
        self.subs.iter().filter(|(_, v)| !v.is_empty()).map(|(k, _)| k.clone()).collect()
    }

    pub fn keys_with_no_subscribers(&self) -> Vec<(String, Option<String>)> {
        self.subs.iter().filter(|(_, v)| v.is_empty()).map(|(k, _)| k.clone()).collect()
    }

    pub fn remove_key(&mut self, provider: &str, path: Option<&str>) {
        let key = make_key(provider, path);
        self.subs.remove(&key);
    }
}

fn make_key(provider: &str, path: Option<&str>) -> SubKey {
    (provider.to_string(), path.map(|s| s.to_string()))
}
