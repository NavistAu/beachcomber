//! Poll-until-condition helpers for integration tests.
//!
//! Replaces the "sleep a fixed amount, then assert" pattern: on a loaded
//! machine a fixed sleep is either too short (flaky) or padded so far past
//! the common case that it taxes every test run. Polling returns the moment
//! the condition holds and only fails once a generous deadline is exhausted,
//! with a message naming what was polled, what was expected, and what was
//! last observed.
//!
//! This file holds the only sleeps permitted under `tests/`: each one is
//! bounded by a deadline and ends the moment its condition holds. Anywhere
//! else, a sleep is a guess about how long the system needs —
//! `tests/suite/no_sleep_in_tests.rs` enforces that.
use std::time::{Duration, Instant};

/// Poll `poll_fn` until `predicate` holds for its return value, or `timeout`
/// elapses.
///
/// Checks immediately (no upfront sleep), then sleeps `interval` between
/// subsequent attempts. `what` names the thing being polled and `expected`
/// describes the condition being waited for; both are folded into the panic
/// message on timeout, along with the last value `poll_fn` produced.
#[allow(dead_code)]
pub fn poll_until<T, F, P>(
    what: &str,
    expected: &str,
    timeout: Duration,
    interval: Duration,
    mut poll_fn: F,
    predicate: P,
) -> T
where
    F: FnMut() -> T,
    P: Fn(&T) -> bool,
    T: std::fmt::Debug,
{
    let deadline = Instant::now() + timeout;
    loop {
        let value = poll_fn();
        if predicate(&value) {
            return value;
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out after {timeout:?} polling {what}: expected {expected}, last observed {value:?}"
            );
        }
        std::thread::sleep(interval);
    }
}

/// Convenience wrapper over [`poll_until`] for the common case of polling
/// until a value equals an expected one.
#[allow(dead_code)]
pub fn poll_until_eq<T>(
    what: &str,
    timeout: Duration,
    interval: Duration,
    poll_fn: impl FnMut() -> T,
    expected_value: T,
) -> T
where
    T: std::fmt::Debug + PartialEq,
{
    let expected = format!("{expected_value:?}");
    poll_until(what, &expected, timeout, interval, poll_fn, |v| {
        *v == expected_value
    })
}

/// Async twin of [`poll_until`] for `#[tokio::test]` bodies. Yields to the
/// runtime between attempts instead of blocking the thread, so the tasks being
/// waited on (server, scheduler) keep making progress under a current-thread
/// runtime. `poll_fn` may await: `async || cache.get_entry("x", None)` or
/// `async || send_recv(&mut stream, req).await` both work.
#[allow(dead_code)]
pub async fn poll_until_async<T, F, P>(
    what: &str,
    expected: &str,
    timeout: Duration,
    interval: Duration,
    mut poll_fn: F,
    predicate: P,
) -> T
where
    F: AsyncFnMut() -> T,
    P: Fn(&T) -> bool,
    T: std::fmt::Debug,
{
    let deadline = Instant::now() + timeout;
    loop {
        let value = poll_fn().await;
        if predicate(&value) {
            return value;
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out after {timeout:?} polling {what}: expected {expected}, last observed {value:?}"
            );
        }
        tokio::time::sleep(interval).await;
    }
}

/// Async twin of [`poll_until_eq`].
#[allow(dead_code)]
pub async fn poll_until_eq_async<T, F>(
    what: &str,
    timeout: Duration,
    interval: Duration,
    poll_fn: F,
    expected_value: T,
) -> T
where
    F: AsyncFnMut() -> T,
    T: std::fmt::Debug + PartialEq,
{
    let expected = format!("{expected_value:?}");
    poll_until_async(what, &expected, timeout, interval, poll_fn, |v| {
        *v == expected_value
    })
    .await
}
