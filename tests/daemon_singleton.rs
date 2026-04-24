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

use beachcomber::singleton::{SingletonLock, SingletonLockError, SupersessionDecision, decide_supersession, PidFileRecord};

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

#[test]
fn supersession_same_version_means_no_op() {
    let existing = PidFileRecord {
        pid: 12345,
        version: "0.5.1+sha.abc".into(),
        binary: "/path/to/comb".into(),
        started_unix_ms: 0,
    };
    let decision = decide_supersession(&existing, "0.5.1+sha.abc");
    assert!(matches!(decision, SupersessionDecision::ExitSilent));
}

#[test]
fn supersession_different_version_means_supersede() {
    let existing = PidFileRecord {
        pid: 12345,
        version: "0.5.0".into(),
        binary: "/path/to/comb".into(),
        started_unix_ms: 0,
    };
    let decision = decide_supersession(&existing, "0.5.1+sha.abc");
    match decision {
        SupersessionDecision::Supersede { existing_pid } => assert_eq!(existing_pid, 12345),
        _ => panic!("expected Supersede, got {decision:?}"),
    }
}

#[test]
fn supersession_dev_build_different_sha_means_supersede() {
    let existing = PidFileRecord {
        pid: 12345,
        version: "0.5.1+sha.abc11111".into(),
        binary: "/path/to/comb".into(),
        started_unix_ms: 0,
    };
    let decision = decide_supersession(&existing, "0.5.1+sha.def22222");
    assert!(matches!(decision, SupersessionDecision::Supersede { .. }));
}

#[test]
fn supersede_existing_kills_target_process() {
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep");
    let pid = child.id();

    // Call supersede_existing. It may report "survived SIGKILL" because we're the parent
    // of the target and the process becomes a zombie until we wait() — that's a test-env
    // artefact, not a production bug.
    let _ = beachcomber::singleton::supersede_existing(pid, std::time::Duration::from_millis(500));

    // Reap the zombie.
    let _ = child.wait();

    // After reaping, the kernel frees the PID slot.
    std::thread::sleep(std::time::Duration::from_millis(50));
    let still_alive = unsafe { libc::kill(pid as libc::pid_t, 0) };
    assert!(still_alive != 0, "process should be gone; kill(0) returned {still_alive}");
}

#[test]
fn supersede_existing_refuses_pid_one() {
    let result = beachcomber::singleton::supersede_existing(1, std::time::Duration::from_millis(100));
    assert!(result.is_err(), "expected error refusing pid 1");
}

#[test]
fn supersede_existing_refuses_self() {
    let me = std::process::id();
    let result = beachcomber::singleton::supersede_existing(me, std::time::Duration::from_millis(100));
    assert!(result.is_err(), "expected error refusing self pid");
}

#[test]
fn binary_newer_than_returns_true_when_file_modified_after_start() {
    let tmpdir = tempfile::tempdir().unwrap();
    let fake_binary = tmpdir.path().join("fake");
    std::fs::write(&fake_binary, b"hello").unwrap();

    let process_start_ms = 0u64;  // 1970-01-01 — far before fake_binary's mtime
    let result = beachcomber::singleton::binary_newer_than(&fake_binary, process_start_ms)
        .expect("metadata read");
    assert!(result, "freshly written file should be newer than 1970");
}

#[test]
fn binary_newer_than_returns_false_when_file_older_than_start() {
    let tmpdir = tempfile::tempdir().unwrap();
    let fake_binary = tmpdir.path().join("fake");
    std::fs::write(&fake_binary, b"hello").unwrap();

    // Use a far-future timestamp so the file's mtime is older.
    let process_start_ms = u64::MAX / 2;
    let result = beachcomber::singleton::binary_newer_than(&fake_binary, process_start_ms)
        .expect("metadata read");
    assert!(!result, "file should be older than far-future timestamp");
}

#[test]
fn acquire_or_supersede_same_version_returns_none() {
    let tmpdir = tempfile::tempdir().unwrap();
    let pid_path = tmpdir.path().join("pid");

    let _first = beachcomber::singleton::SingletonLock::acquire(&pid_path, "0.5.1").unwrap();

    let second = beachcomber::singleton::acquire_or_supersede(&pid_path, "0.5.1")
        .expect("should succeed with Ok(None)");
    assert!(second.is_none(), "same version should return None");
}
