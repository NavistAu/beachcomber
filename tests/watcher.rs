use shellstate::watcher::FsWatcher;
use std::fs;
use tempfile::TempDir;
use tokio::time::{timeout, Duration};

#[tokio::test]
async fn watcher_detects_file_creation() {
    let tmp = TempDir::new().unwrap();
    let (mut watcher, mut rx) = FsWatcher::new().expect("Failed to create watcher");
    watcher.watch(tmp.path()).expect("Failed to watch directory");

    fs::write(tmp.path().join("test.txt"), "hello").unwrap();

    let event = timeout(Duration::from_secs(2), rx.recv()).await;
    assert!(event.is_ok(), "Should receive event within timeout");
    let paths = event.unwrap().unwrap();
    assert!(!paths.is_empty(), "Event should have affected paths");
}

#[tokio::test]
async fn watcher_detects_file_modification() {
    let tmp = TempDir::new().unwrap();
    let file_path = tmp.path().join("test.txt");
    fs::write(&file_path, "initial").unwrap();

    let (mut watcher, mut rx) = FsWatcher::new().expect("Failed to create watcher");
    watcher.watch(tmp.path()).expect("Failed to watch directory");

    tokio::time::sleep(Duration::from_millis(100)).await;
    while rx.try_recv().is_ok() {}

    fs::write(&file_path, "modified").unwrap();

    let event = timeout(Duration::from_secs(2), rx.recv()).await;
    assert!(event.is_ok(), "Should receive modification event");
}

#[tokio::test]
async fn watcher_unwatch_stops_events() {
    let tmp = TempDir::new().unwrap();
    let (mut watcher, mut rx) = FsWatcher::new().expect("Failed to create watcher");

    watcher.watch(tmp.path()).expect("Failed to watch directory");
    watcher.unwatch(tmp.path()).expect("Failed to unwatch");

    fs::write(tmp.path().join("test.txt"), "hello").unwrap();

    let result = timeout(Duration::from_millis(500), rx.recv()).await;
    assert!(result.is_err(), "Should NOT receive events after unwatch");
}

#[tokio::test]
async fn watcher_multiple_paths() {
    let tmp1 = TempDir::new().unwrap();
    let tmp2 = TempDir::new().unwrap();

    let (mut watcher, mut rx) = FsWatcher::new().expect("Failed to create watcher");
    watcher.watch(tmp1.path()).expect("Failed to watch dir 1");
    watcher.watch(tmp2.path()).expect("Failed to watch dir 2");

    fs::write(tmp1.path().join("a.txt"), "a").unwrap();

    let event = timeout(Duration::from_secs(2), rx.recv()).await;
    assert!(event.is_ok(), "Should receive event from first watched dir");
}
