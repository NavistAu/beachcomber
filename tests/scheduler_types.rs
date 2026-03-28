use beachcomber::scheduler::TriggerSet;
use beachcomber::protocol::SubscribeTriggers;

#[test]
fn trigger_set_from_protocol_with_watch_and_poll() {
    let proto = SubscribeTriggers { watch: true, poll: Some("30s".to_string()) };
    let ts = TriggerSet::from_protocol(&proto);
    assert!(ts.watch);
    assert_eq!(ts.poll_secs, Some(30));
}

#[test]
fn trigger_set_from_protocol_minutes() {
    let proto = SubscribeTriggers { watch: false, poll: Some("5m".to_string()) };
    let ts = TriggerSet::from_protocol(&proto);
    assert!(!ts.watch);
    assert_eq!(ts.poll_secs, Some(300));
}

#[test]
fn trigger_set_from_protocol_no_suffix() {
    let proto = SubscribeTriggers { watch: false, poll: Some("60".to_string()) };
    let ts = TriggerSet::from_protocol(&proto);
    assert_eq!(ts.poll_secs, Some(60));
}

#[test]
fn trigger_set_from_protocol_none() {
    let proto = SubscribeTriggers { watch: false, poll: None };
    let ts = TriggerSet::from_protocol(&proto);
    assert!(!ts.watch);
    assert_eq!(ts.poll_secs, None);
}

#[test]
fn trigger_set_from_protocol_invalid() {
    let proto = SubscribeTriggers { watch: false, poll: Some("abc".to_string()) };
    let ts = TriggerSet::from_protocol(&proto);
    assert_eq!(ts.poll_secs, None, "Invalid string should parse to None");
}
