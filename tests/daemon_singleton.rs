//! Integration tests for the daemon singleton property and socket-path resolution.

use beachcomber::config::Config;
use std::path::PathBuf;

#[test]
fn resolve_socket_path_prefers_explicit_override() {
    let mut cfg = Config::default();
    cfg.daemon.socket_path = Some("/custom/path".into());
    assert_eq!(cfg.resolve_socket_path(), PathBuf::from("/custom/path"));
}

#[test]
fn resolve_socket_path_falls_back_to_tmp_not_tmpdir() {
    let cfg = Config::default();
    // Preserve and mutate env.
    let old_xdg = std::env::var_os("XDG_RUNTIME_DIR");
    let old_tmp = std::env::var_os("TMPDIR");
    unsafe {
        std::env::remove_var("XDG_RUNTIME_DIR");
        std::env::set_var("TMPDIR", "/per/shell/tmpdir");  // MUST be ignored now
    }
    let path = cfg.resolve_socket_path();
    unsafe {
        match old_xdg {
            Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        };
        match old_tmp {
            Some(v) => std::env::set_var("TMPDIR", v),
            None => std::env::remove_var("TMPDIR"),
        };
    }

    let path_str = path.to_string_lossy();
    assert!(
        path_str.starts_with("/tmp/beachcomber-"),
        "expected /tmp/beachcomber-* prefix, got {path_str}"
    );
    assert!(path_str.ends_with("/sock"), "expected /sock suffix, got {path_str}");
}

#[test]
fn resolve_socket_path_uses_xdg_runtime_dir_when_set() {
    let cfg = Config::default();
    let old_xdg = std::env::var_os("XDG_RUNTIME_DIR");
    let old_tmp = std::env::var_os("TMPDIR");
    unsafe {
        std::env::set_var("XDG_RUNTIME_DIR", "/run/user/501");
        std::env::set_var("TMPDIR", "/nowhere");
    }
    let path = cfg.resolve_socket_path();
    unsafe {
        match old_xdg {
            Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        };
        match old_tmp {
            Some(v) => std::env::set_var("TMPDIR", v),
            None => std::env::remove_var("TMPDIR"),
        };
    }
    assert_eq!(path, PathBuf::from("/run/user/501/beachcomber/sock"));
}

use beachcomber::singleton::{SingletonLock, SingletonLockError};

#[test]
fn singleton_lock_creates_pid_file_and_holds_flock() {
    let tmpdir = tempfile::tempdir().unwrap();
    let pid_path = tmpdir.path().join("pid");

    let lock = SingletonLock::acquire(&pid_path, "0.5.1+sha.abc").expect("first acquire succeeds");
    assert!(pid_path.exists(), "pid file should be created");

    let contents = std::fs::read_to_string(&pid_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
    assert_eq!(parsed["pid"].as_u64().unwrap(), std::process::id() as u64);
    assert_eq!(parsed["version"].as_str().unwrap(), "0.5.1+sha.abc");
    assert!(parsed["binary"].is_string());
    assert!(parsed["started_unix_ms"].is_number());

    drop(lock);
    assert!(!pid_path.exists(), "pid file should be deleted on drop");
}

#[test]
fn singleton_lock_contested_by_same_process_fails() {
    let tmpdir = tempfile::tempdir().unwrap();
    let pid_path = tmpdir.path().join("pid");

    let _first = SingletonLock::acquire(&pid_path, "0.5.1+sha.abc").unwrap();
    let second = SingletonLock::acquire(&pid_path, "0.5.1+sha.abc");
    assert!(matches!(second, Err(SingletonLockError::AlreadyHeld { .. })));
}
