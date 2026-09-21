use beachcomber::config::parse_duration;
use std::time::Duration;

#[test]
fn accepts_whole_seconds() {
    assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
    assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
    assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
}

#[test]
fn accepts_ms_that_are_whole_seconds() {
    assert_eq!(parse_duration("1000ms"), Some(Duration::from_secs(1)));
    assert_eq!(parse_duration("2000ms"), Some(Duration::from_secs(2)));
}

#[test]
fn rejects_sub_second_ms() {
    assert_eq!(parse_duration("0ms"), None);
    assert_eq!(parse_duration("500ms"), None);
    assert_eq!(parse_duration("1ms"), None);
    assert_eq!(parse_duration("999ms"), None);
}

#[test]
fn rejects_non_whole_second_ms() {
    assert_eq!(parse_duration("1500ms"), None);
    assert_eq!(parse_duration("2500ms"), None);
}

#[test]
fn empty_or_garbage_returns_none() {
    assert_eq!(parse_duration(""), None);
    assert_eq!(parse_duration("abc"), None);
    assert_eq!(parse_duration("5x"), None);
}
