use beachcomber::config::Config;
use beachcomber::daemon;
use tempfile::TempDir;
use tokio::net::UnixStream;

#[tokio::test]
async fn daemon_shuts_down_on_cancel() {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let config = Config::default();

    let token = tokio_util::sync::CancellationToken::new();
    let (handle, ready) =
        daemon::start_in_process_polling_with(sock.clone(), config, token.clone(), None);
    ready.await.expect("daemon ready");
    assert!(
        UnixStream::connect(&sock).await.is_ok(),
        "Daemon should be running"
    );

    token.cancel();

    let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    assert!(result.is_ok(), "Daemon should shut down within timeout");
}
