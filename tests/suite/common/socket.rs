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
