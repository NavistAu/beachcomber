//! Integration test: daemon gracefully shuts down when its binary is modified.

/// Canon §"Self-supervision": the mtime poll is the guarantee — it fires even
/// when the fs-event backend delivers nothing (the sandboxed-daemon failure
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

    // Bump mtime strictly past the watch's start time, immune to clock granularity.
    std::thread::sleep(std::time::Duration::from_millis(50));
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(&fake_bin)
        .unwrap();
    f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(2))
        .unwrap();

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
        .args(["daemon", "--socket", sock.to_str().unwrap()])
        .spawn()
        .expect("spawn daemon");

    // Wait for socket to exist.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !sock.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(sock.exists(), "daemon didn't bind socket in 5s");

    // Touch the binary (changes mtime).
    std::process::Command::new("touch")
        .arg(&target)
        .status()
        .expect("touch");

    // Daemon should exit within a few seconds (200ms debounce + drain).
    let wait_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < wait_deadline {
        if let Ok(Some(_status)) = child.try_wait() {
            let _ = child.wait();
            return; // success
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    let _ = child.kill();
    let _ = child.wait();
    panic!("daemon did not exit after binary was touched");
}
