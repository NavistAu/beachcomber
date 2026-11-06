// Option A: copied from libbeachcomber/tests/common/socket.rs.
// Both crates need an IsolatedSocket in their integration-test scope; duplicating the
// 12-line struct is simpler than bridging crate test boundaries with #[path = "..."].
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

#[allow(dead_code)]
pub struct IsolatedSocket {
    pub dir: TempDir,
    pub path: PathBuf,
}

impl IsolatedSocket {
    /// Create an isolated socket at `<tempdir>/beachcomber/sock`.
    ///
    /// Tests point a process at exactly this socket via `BEACHCOMBER_SOCKET`
    /// (or `--socket`); canonical resolution consults no session-scoped
    /// environment, so a tempdir path can only reach a process explicitly.
    // Used by multiple integration test binaries via the shared common module.
    #[allow(dead_code)]
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        // The daemon's server creates parent dirs via `create_dir_all`, so we
        // don't need to pre-create the `beachcomber/` subdirectory here.
        let path = dir.path().join("beachcomber").join("sock");
        Self { dir, path }
    }
}

/// One request to the daemon at `sock`, on a connection of its own, and its
/// answer.
#[allow(dead_code)]
pub fn ask(sock: &Path, request: Value) -> Value {
    let mut stream = UnixStream::connect(sock).expect("connect");
    let mut line = request.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes()).unwrap();
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer).unwrap();
    serde_json::from_str(&answer).expect("a JSON answer")
}

/// The watches the daemon at `sock` reports (`introspect watches`), as
/// `(path, the rest of the entry)`, less `asked_secs_ago`, a clock reading.
#[allow(dead_code)]
pub fn watches(sock: &Path) -> Vec<(String, Value)> {
    ask(
        sock,
        serde_json::json!({"op": "introspect", "subject": "watches"}),
    )["data"]["watches"]
        .as_array()
        .expect("watches")
        .iter()
        .map(|w| {
            let mut rest = w.clone();
            rest.as_object_mut().unwrap().remove("asked_secs_ago");
            let path = rest
                .as_object_mut()
                .unwrap()
                .remove("path")
                .and_then(|p| p.as_str().map(str::to_string))
                .expect("a path");
            (path, rest)
        })
        .collect()
}

/// Write `body` to `path`, a file the memo at `cache_key` (`provider:cache.
/// field`) watches, and wait until the daemon at `sock` has heard it: the
/// memo's hash is gone. The watcher hands a change over a moment after it is
/// made, and a hash dropped after the next read wrote a new memo makes the
/// read after that run again: a count of runs would be one too many.
#[allow(dead_code)]
pub fn rewrite_watched(sock: &Path, cache_key: &str, path: &Path, body: &str) {
    use crate::common::poll::poll_until;
    use std::time::Duration;
    let memo = || ask(sock, serde_json::json!({"op": "get", "key": cache_key}))["data"].clone();
    // Every path the memo names is watched first, so the write is an event.
    poll_until(
        &format!("{cache_key} and introspect watches"),
        "a hash, and each path the memo watches Active",
        Duration::from_secs(10),
        Duration::from_millis(5),
        || (memo(), watches(sock)),
        |(memo, watched)| {
            memo.get("hash").is_some()
                && memo["watch"].as_array().is_some_and(|paths| {
                    paths.iter().all(|p| {
                        watched.iter().any(|(w, state)| {
                            Some(w.as_str()) == p.as_str() && state["state"] == "active"
                        })
                    })
                })
        },
    );
    std::fs::write(path, body).unwrap();
    poll_until(
        cache_key,
        "the memo's record without its hash",
        Duration::from_secs(10),
        Duration::from_millis(5),
        memo,
        |memo| memo.get("hash").is_none(),
    );
}
