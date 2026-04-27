use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

#[test]
fn connect_retries_succeed_after_brief_outage() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("sock");

    // Spawn a binder thread that binds after 400ms.
    let sock_clone = sock.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        let listener = UnixListener::bind(&sock_clone).unwrap();
        // Accept and immediately close one connection, then hold open briefly.
        if let Ok((conn, _)) = listener.accept() {
            drop(conn);
        }
        std::thread::sleep(Duration::from_secs(5));
    });

    let start = Instant::now();
    let result = libbeachcomber::connect_with_retry(&sock);
    let elapsed = start.elapsed();

    assert!(
        result.is_ok(),
        "connect_with_retry should succeed after retry"
    );
    assert!(
        elapsed >= Duration::from_millis(250),
        "should have retried at least once; elapsed={:?}",
        elapsed
    );
}

#[test]
fn connect_retries_exhaust_after_three_attempts() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("nosock");

    let start = Instant::now();
    let result = libbeachcomber::connect_with_retry(&sock);
    let elapsed = start.elapsed();

    assert!(
        result.is_err(),
        "connect_with_retry should fail when nothing binds"
    );
    // 250 + 500 + 1000 = 1750ms minimum (backoffs before final attempt).
    assert!(
        elapsed >= Duration::from_millis(1700),
        "should wait through all retries; elapsed={:?}",
        elapsed
    );
}
