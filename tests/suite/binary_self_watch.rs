//! Integration test: daemon gracefully shuts down when its binary is modified.

/// Canon §"Self-supervision": the mtime poll is the guarantee — it fires even
/// when the fs-event backend delivers nothing (the silent-delivery failure
/// mode that made leaked daemons immortal).
#[test]
fn poll_catches_binary_replacement_when_fs_events_are_dead() {
    let tmp = tempfile::tempdir().unwrap();
    let fake_bin = tmp.path().join("comb");
    std::fs::write(&fake_bin, b"v1").unwrap();

    let (tx, rx) = std::sync::mpsc::channel();
    beachcomber::singleton::spawn_binary_self_watch_with(
        fake_bin.clone(),
        std::time::Duration::from_millis(10),
        false, // fs events dead — the poll is the only mechanism
        move || {
            let _ = tx.send(());
        },
    );

    crate::common::mtime::touch_future(&fake_bin);

    rx.recv_timeout(std::time::Duration::from_secs(3))
        .expect("mtime poll fired on_change");
}
