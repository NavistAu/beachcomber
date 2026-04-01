use beachcomber::daemon;
use std::path::PathBuf;
use tempfile::TempDir;
use tokio::net::UnixStream;

#[test]
fn pid_file_path() {
    let sock = PathBuf::from("/tmp/beachcomber-501/sock");
    let pid = daemon::pid_path_for_socket(&sock);
    assert_eq!(pid, PathBuf::from("/tmp/beachcomber-501/daemon.pid"));
}

#[test]
fn is_daemon_responsive_when_no_socket() {
    let path = PathBuf::from("/tmp/nonexistent-beachcomber-test/sock");
    assert!(
        !daemon::is_daemon_running(&path),
        "Should return false when socket doesn't exist"
    );
}

#[tokio::test]
async fn socket_activation_starts_daemon() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");

    let config = beachcomber::config::Config::default();
    let handle = daemon::start_in_process(sock.clone(), config);

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let stream = UnixStream::connect(&sock).await;
    assert!(stream.is_ok(), "Should connect to daemon socket");

    handle.abort();
}
