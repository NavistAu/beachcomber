use std::os::unix::net::UnixListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// A binder thread that holds a Unix listener open until the guard is dropped.
/// Dropping the guard sets a stop flag and joins the thread.
struct BinderGuard {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl BinderGuard {
    /// Spawn a thread that binds `sock` after `delay`, accepts one connection,
    /// then waits for the stop flag before exiting.
    fn spawn(sock: std::path::PathBuf, delay: Duration) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            std::thread::sleep(delay);
            let listener = UnixListener::bind(&sock).unwrap();
            // Accept one connection (the retry client) and close it.
            listener.set_nonblocking(false).ok();
            if let Ok((conn, _)) = listener.accept() {
                drop(conn);
            }
            // Hold the listener open until the guard signals us to stop.
            while !stop_clone.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        BinderGuard {
            stop,
            thread: Some(handle),
        }
    }
}

impl Drop for BinderGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}

#[test]
fn connect_retries_succeed_after_brief_outage() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("sock");

    // Spawn a binder thread that binds after 400ms.
    let _guard = BinderGuard::spawn(sock.clone(), Duration::from_millis(400));

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
    // _guard drops here, joining the binder thread cleanly.
}

#[test]
fn connect_retries_exhaust_at_the_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("nosock");

    let start = Instant::now();
    let result = libbeachcomber::connect_with_retry_for(&sock, Duration::from_millis(200));
    let elapsed = start.elapsed();

    assert!(
        result.is_err(),
        "connect_with_retry_for should fail when nothing binds"
    );
    // The last sleep is clipped to the deadline, so the final attempt lands
    // at the budget, not past the next doubling.
    assert!(
        elapsed >= Duration::from_millis(200),
        "should keep trying until the budget is spent; elapsed={:?}",
        elapsed
    );
}

#[test]
fn zero_budget_is_a_single_attempt() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("nosock");

    let start = Instant::now();
    let result = libbeachcomber::connect_with_retry_for(&sock, Duration::ZERO);
    let elapsed = start.elapsed();

    assert_eq!(
        result.err().map(|e| e.kind()),
        Some(std::io::ErrorKind::NotFound)
    );
    assert!(
        elapsed < Duration::from_millis(100),
        "no budget means no sleeping; elapsed={:?}",
        elapsed
    );
}
