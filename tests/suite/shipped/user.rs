//! `defaults/user.toml`: the daemon's user, from `sys`.

use super::scenario;
use serde_json::json;

/// This process's uid and its login name.
fn this_user() -> (libc::uid_t, String) {
    // SAFETY: `getuid` cannot fail.
    let uid = unsafe { libc::getuid() };
    // SAFETY: one thread reads the entry, and copies it out at once.
    let pwd = unsafe { libc::getpwuid(uid) };
    assert!(!pwd.is_null(), "uid {uid} has an entry");
    // SAFETY: a found entry's name is a NUL-terminated string.
    let name = unsafe { std::ffi::CStr::from_ptr((*pwd).pw_name) }
        .to_string_lossy()
        .into_owned();
    (uid, name)
}

#[test]
fn user_provider_executes() {
    let s = scenario();
    let (uid, name) = this_user();
    assert_eq!(
        s.get("user", &s.project),
        Some(json!({"name": name, "uid": uid}))
    );
}

/// The deliberate difference: polled every 10 minutes while read, where the
/// Rust Source ran once.
#[test]
fn user_is_polled_every_ten_minutes() {
    let s = scenario();
    // A cache leaf describes a record: the read makes it.
    assert!(s.get("user.uid", &s.project).is_some());
    let leaf = s.get("user:cache.uid", &s.project).expect("a cache leaf");
    assert_eq!(
        leaf["poll"],
        json!({"interval": "600s", "count": 6}),
        "{leaf}"
    );
}
