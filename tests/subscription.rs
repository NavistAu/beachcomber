use beachcomber::subscription::SubscriptionManager;
use beachcomber::scheduler::TriggerSet;

fn watch_triggers() -> TriggerSet {
    TriggerSet { watch: true, poll_secs: None }
}

fn poll_triggers(secs: u64) -> TriggerSet {
    TriggerSet { watch: false, poll_secs: Some(secs) }
}

fn watch_and_poll(secs: u64) -> TriggerSet {
    TriggerSet { watch: true, poll_secs: Some(secs) }
}

#[test]
fn subscribe_creates_entry() {
    let mut mgr = SubscriptionManager::new();
    mgr.subscribe(1, "git", Some("/project"), watch_triggers());
    assert_eq!(mgr.subscriber_count("git", Some("/project")), 1);
}

#[test]
fn subscribe_multiple_consumers() {
    let mut mgr = SubscriptionManager::new();
    mgr.subscribe(1, "git", Some("/project"), watch_triggers());
    mgr.subscribe(2, "git", Some("/project"), poll_triggers(10));
    assert_eq!(mgr.subscriber_count("git", Some("/project")), 2);
}

#[test]
fn unsubscribe_removes_consumer() {
    let mut mgr = SubscriptionManager::new();
    mgr.subscribe(1, "git", Some("/project"), watch_triggers());
    mgr.subscribe(2, "git", Some("/project"), poll_triggers(10));
    mgr.unsubscribe(1, "git", Some("/project"));
    assert_eq!(mgr.subscriber_count("git", Some("/project")), 1);
}

#[test]
fn disconnect_removes_all_consumer_subs() {
    let mut mgr = SubscriptionManager::new();
    mgr.subscribe(1, "git", Some("/project-a"), watch_triggers());
    mgr.subscribe(1, "git", Some("/project-b"), watch_triggers());
    mgr.subscribe(1, "hostname", None, poll_triggers(30));
    mgr.subscribe(2, "git", Some("/project-a"), watch_triggers());

    mgr.disconnect(1);

    assert_eq!(mgr.subscriber_count("git", Some("/project-a")), 1, "Consumer 2 still subscribed");
    assert_eq!(mgr.subscriber_count("git", Some("/project-b")), 0, "No subscribers left");
    assert_eq!(mgr.subscriber_count("hostname", None), 0, "No subscribers left");
}

#[test]
fn effective_triggers_union_watch() {
    let mut mgr = SubscriptionManager::new();
    mgr.subscribe(1, "git", Some("/project"), TriggerSet { watch: false, poll_secs: Some(30) });
    mgr.subscribe(2, "git", Some("/project"), TriggerSet { watch: true, poll_secs: None });

    let effective = mgr.effective_triggers("git", Some("/project")).unwrap();
    assert!(effective.watch, "Any consumer wanting watch should activate it");
    assert_eq!(effective.poll_secs, Some(30), "Poll from consumer 1");
}

#[test]
fn effective_triggers_shortest_poll() {
    let mut mgr = SubscriptionManager::new();
    mgr.subscribe(1, "git", Some("/project"), poll_triggers(30));
    mgr.subscribe(2, "git", Some("/project"), poll_triggers(10));
    mgr.subscribe(3, "git", Some("/project"), poll_triggers(60));

    let effective = mgr.effective_triggers("git", Some("/project")).unwrap();
    assert_eq!(effective.poll_secs, Some(10), "Should use shortest poll interval");
}

#[test]
fn effective_triggers_with_floor() {
    let mut mgr = SubscriptionManager::new();
    mgr.subscribe(1, "git", Some("/project"), poll_triggers(1));

    let effective = mgr.effective_triggers_with_floor("git", Some("/project"), 5).unwrap();
    assert_eq!(effective.poll_secs, Some(5), "Floor should enforce minimum");
}

#[test]
fn effective_triggers_none_when_no_subscribers() {
    let mgr = SubscriptionManager::new();
    assert!(mgr.effective_triggers("git", Some("/project")).is_none());
}

#[test]
fn all_subscribed_keys() {
    let mut mgr = SubscriptionManager::new();
    mgr.subscribe(1, "git", Some("/project-a"), watch_triggers());
    mgr.subscribe(1, "hostname", None, poll_triggers(30));
    mgr.subscribe(2, "git", Some("/project-b"), watch_triggers());

    let keys = mgr.all_keys();
    assert_eq!(keys.len(), 3);
}

#[test]
fn keys_with_no_subscribers() {
    let mut mgr = SubscriptionManager::new();
    mgr.subscribe(1, "git", Some("/project"), watch_triggers());
    mgr.unsubscribe(1, "git", Some("/project"));

    let orphans = mgr.keys_with_no_subscribers();
    assert_eq!(orphans.len(), 1, "Should have one orphaned key");
    assert_eq!(orphans[0], ("git".to_string(), Some("/project".to_string())));
}
