use beachcomber::boundaries::socket::{RealSocketDiscovery, SocketDiscovery};

#[test]
fn tmpdir_returns_existing_path() {
    let d = RealSocketDiscovery;
    let path = d.tmpdir();
    assert!(
        path.exists(),
        "tmpdir() returned a non-existent path: {path:?}"
    );
}

#[test]
fn xdg_runtime_dir_matches_env() {
    let d = RealSocketDiscovery;
    let via_trait = d.xdg_runtime_dir();
    let via_env = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .map(std::path::PathBuf::from);
    assert_eq!(via_trait, via_env);
}

#[test]
fn default_socket_path_is_stable_per_user() {
    let d = RealSocketDiscovery;
    // Resolution consults no session-scoped environment, so the result is
    // deterministic: /tmp/beachcomber-<uid>/sock regardless of env state.
    let uid = unsafe { libc::getuid() };
    assert_eq!(
        d.default_socket_path(),
        std::path::PathBuf::from(format!("/tmp/beachcomber-{uid}/sock"))
    );
}
