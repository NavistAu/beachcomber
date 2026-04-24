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
