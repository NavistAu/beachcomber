//! Poll-until-condition helper for integration tests.
//!
//! Replaces the "sleep a fixed amount, then assert" pattern: on a loaded
//! machine a fixed sleep is either too short (flaky) or padded so far past
//! the common case that it taxes every test run. Polling returns the moment
//! the condition holds and only fails once a generous deadline is exhausted,
//! with a message naming what was polled, what was expected, and what was
//! last observed.
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
