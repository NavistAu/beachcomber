//! Integration test: daemon gracefully shuts down when its binary is modified.

mod common;

use common::poll::poll_until;

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
        std::time::Duration::from_millis(100),
        false, // fs events dead — the poll is the only mechanism
        move || {
            let _ = tx.send(());
        },
    );

    common::mtime::touch_future(&fake_bin);

    rx.recv_timeout(std::time::Duration::from_secs(3))
        .expect("mtime poll fired on_change");
}

#[tokio::test]
#[ignore]
async fn daemon_shuts_down_when_binary_touched() {
    let tmpdir = tempfile::tempdir().unwrap();
    let target = tmpdir.path().join("comb");

    // Assume `cargo build --release` has been run; copy the binary.
    let src = std::path::Path::new("./target/release/comb");
    if !src.exists() {
        eprintln!("skipping: ./target/release/comb missing — run `cargo build --release` first");
        return;
    }
    std::fs::copy(src, &target).expect("copy binary");

    let sock = tmpdir.path().join("sock");
    let mut child = std::process::Command::new(&target)
        .env("HOME", tmpdir.path())
        .env("XDG_CONFIG_HOME", tmpdir.path())
        .env("XDG_STATE_HOME", tmpdir.path().join(".local/state"))
        .args(["daemon", "--socket", sock.to_str().unwrap()])
        .spawn()
        .expect("spawn daemon");

    // Wait for socket to exist.
    poll_until(
        &format!("daemon socket at {sock:?}"),
        "the daemon to bind it",
        std::time::Duration::from_secs(5),
        std::time::Duration::from_millis(50),
        || sock.exists(),
        |exists| *exists,
    );

    // Touch the binary (changes mtime).
    std::process::Command::new("touch")
        .arg(&target)
        .status()
        .expect("touch");

    // Daemon should exit within a few seconds (200ms debounce + drain).
    // `catch_unwind` so the child is still reaped when the poll times out —
    // this file's whole subject is daemons that outlive their business.
    let exited = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        poll_until(
            "spawned daemon's exit status",
            "the daemon to exit after its binary was touched",
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(100),
            || child.try_wait().ok().flatten(),
            Option::is_some,
        )
    }));
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        exited.is_ok(),
        "daemon did not exit after binary was touched"
    );
}
