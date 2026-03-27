use std::path::PathBuf;
use tokio::sync::mpsc;

/// Messages sent from the Server to the Scheduler.
#[derive(Debug)]
pub enum SchedulerMessage {
    Subscribe {
        consumer_id: u64,
        provider: String,
        path: Option<String>,
        triggers: TriggerSet,
    },
    Unsubscribe {
        consumer_id: u64,
        provider: String,
        path: Option<String>,
    },
    ConsumerDisconnected {
        consumer_id: u64,
    },
    Poke {
        provider: String,
        path: Option<String>,
    },
    FsEvent {
        paths: Vec<PathBuf>,
    },
    Shutdown,
}

/// The set of triggers a consumer requests for a subscription.
#[derive(Debug, Clone)]
pub struct TriggerSet {
    pub watch: bool,
    pub poll_secs: Option<u64>,
}

impl TriggerSet {
    pub fn from_protocol(triggers: &crate::protocol::SubscribeTriggers) -> Self {
        let poll_secs = triggers.poll.as_ref().and_then(|s| parse_duration_secs(s));
        Self {
            watch: triggers.watch,
            poll_secs,
        }
    }
}

/// Parse a duration string like "30s", "5m", "1h" into seconds.
fn parse_duration_secs(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num_str, multiplier) = if s.ends_with('s') {
        (&s[..s.len() - 1], 1u64)
    } else if s.ends_with('m') {
        (&s[..s.len() - 1], 60)
    } else if s.ends_with('h') {
        (&s[..s.len() - 1], 3600)
    } else {
        (s, 1)
    };
    num_str.trim().parse::<u64>().ok().map(|n| n * multiplier)
}

/// Handle for sending messages to the scheduler.
#[derive(Clone)]
pub struct SchedulerHandle {
    tx: mpsc::Sender<SchedulerMessage>,
}

impl SchedulerHandle {
    pub fn new(tx: mpsc::Sender<SchedulerMessage>) -> Self {
        Self { tx }
    }

    pub async fn send(&self, msg: SchedulerMessage) {
        let _ = self.tx.send(msg).await;
    }
}
