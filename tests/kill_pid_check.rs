use beachcomber::pid_check::pid_matches_our_daemon;

#[test]
fn accepts_matching_comm() {
    assert!(pid_matches_our_daemon(
        true, /* kill0_ok */
        Some("comb".into())
    ));
    assert!(pid_matches_our_daemon(true, Some("comb\n".into())));
}

#[test]
fn rejects_missing_or_wrong_comm() {
    assert!(!pid_matches_our_daemon(true, None));
    assert!(!pid_matches_our_daemon(true, Some("bash".into())));
    assert!(!pid_matches_our_daemon(true, Some("".into())));
}

#[test]
fn rejects_dead_pid() {
    assert!(!pid_matches_our_daemon(false, Some("comb".into())));
}
