use shellstate::config::Config;
use shellstate::daemon;
use tempfile::TempDir;
use tokio::net::UnixStream;

#[tokio::test]
async fn daemon_shuts_down_on_cancel() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let config = Config::default();

    let token = tokio_util::sync::CancellationToken::new();
    let handle = daemon::start_in_process_with_cancel(sock.clone(), config, token.clone());

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(UnixStream::connect(&sock).await.is_ok(), "Daemon should be running");

    token.cancel();

    let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    assert!(result.is_ok(), "Daemon should shut down within timeout");
}
