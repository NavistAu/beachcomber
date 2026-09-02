use beachcomber::watcher::FsWatcher;
use std::fs;
use tempfile::TempDir;
use tokio::time::{Duration, timeout};

// These tests use FsWatcher::new_polling rather than FsWatcher::new so they
// are decoupled from the host's kernel filesystem-notification stack. On
// macOS, FSEvents can be disabled for processes without Full Disk Access (sandboxed
// CI, editors without TCC prompts answered, terminal apps without the
// entitlement). Polling works everywhere, at the cost of a short poll interval.
// Production code still uses FsWatcher::new, which picks the kernel-native
// backend (FSEvents on macOS, inotify on Linux).
const POLL_INTERVAL: Duration = Duration::from_millis(100);

// Polling fires a full directory scan on each tick; the first real event is
// typically delivered within 2–3 poll intervals once a change has landed on
// disk. A 5s outer timeout gives headroom on slow machines.
const EVENT_TIMEOUT: Duration = Duration::from_secs(5);

fn canonical_tempdir() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let canonical = tmp.path().canonicalize().expect("canonicalize temp dir");
    (tmp, canonical)
}

#[tokio::test]
async fn watcher_detects_file_creation() {
    let (_tmp, dir) = canonical_tempdir();
    let (mut watcher, mut rx) =
        FsWatcher::new_polling(POLL_INTERVAL).expect("Failed to create watcher");
    watcher.watch(&dir).expect("Failed to watch directory");

    fs::write(dir.join("test.txt"), "hello").unwrap();

    let event = timeout(EVENT_TIMEOUT, rx.recv()).await;
    assert!(event.is_ok(), "Should receive event within timeout");
    let paths = event.unwrap().unwrap();
    assert!(!paths.is_empty(), "Event should have affected paths");
}

#[tokio::test]
async fn watcher_detects_file_modification() {
    let (_tmp, dir) = canonical_tempdir();
    let file_path = dir.join("test.txt");
    fs::write(&file_path, "initial").unwrap();

    let (mut watcher, mut rx) =
        FsWatcher::new_polling(POLL_INTERVAL).expect("Failed to create watcher");
    watcher.watch(&dir).expect("Failed to watch directory");

    // PollWatcher compares each scan against the previous one to detect
    // changes, and `watch()` takes that baseline snapshot synchronously before
    // it returns — so "initial" is already recorded here and the rewrite below
    // cannot race it. Nothing to wait for; wait on the event itself.
    fs::write(&file_path, "modified-distinct-content").unwrap();

    let paths = timeout(EVENT_TIMEOUT, rx.recv())
        .await
        .expect("Should receive modification event")
        .expect("watcher channel should stay open");
    assert!(
        paths.contains(&file_path),
        "modification event should name the modified file, got {paths:?}"
    );
}

#[tokio::test]
async fn watcher_unwatch_stops_events() {
    let (_tmp, dir) = canonical_tempdir();
    let (_fence_tmp, fence_dir) = canonical_tempdir();
    let (mut watcher, mut rx) =
        FsWatcher::new_polling(POLL_INTERVAL).expect("Failed to create watcher");

    watcher.watch(&dir).expect("Failed to watch directory");
    watcher.unwatch(&dir).expect("Failed to unwatch");

    fs::write(dir.join("test.txt"), "hello").unwrap();

    // "No event ever arrives" has no condition to wait on, so fence it: one
    // poll tick scans every watched path, so the tick that reports the fence
    // dir's write is a tick that would already have reported `dir`'s — and
    // that event, being older, would sit ahead of the fence's in the queue.
    watcher
        .watch(&fence_dir)
        .expect("Failed to watch fence directory");
    fs::write(fence_dir.join("fence.txt"), "fence").unwrap();

    let paths = timeout(EVENT_TIMEOUT, rx.recv())
        .await
        .expect("fence event should arrive")
        .expect("watcher channel should stay open");
    assert!(
        paths.iter().all(|p| p.starts_with(&fence_dir)),
        "Should NOT receive events after unwatch; got {paths:?}"
    );
    while let Ok(paths) = rx.try_recv() {
        assert!(
            paths.iter().all(|p| p.starts_with(&fence_dir)),
            "Should NOT receive events after unwatch; got {paths:?}"
        );
    }
}

#[tokio::test]
async fn watcher_multiple_paths() {
    let (_tmp1, dir1) = canonical_tempdir();
    let (_tmp2, dir2) = canonical_tempdir();

    let (mut watcher, mut rx) =
        FsWatcher::new_polling(POLL_INTERVAL).expect("Failed to create watcher");
    watcher.watch(&dir1).expect("Failed to watch dir 1");
    watcher.watch(&dir2).expect("Failed to watch dir 2");

    fs::write(dir1.join("a.txt"), "a").unwrap();

    let event = timeout(EVENT_TIMEOUT, rx.recv()).await;
    assert!(event.is_ok(), "Should receive event from first watched dir");
}
