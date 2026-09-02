//! Sleeps in tests are banned.
//!
//! A fixed sleep is a guess about how long the system needs. On a loaded
//! machine it is too short and the test flakes; on a fast one it is too long
//! and every run pays for it. Wait on the condition instead:
//!
//! - a server: `Server::spawn()` binds before it returns — connect immediately
//! - an in-process daemon: `start_in_process_polling` returns a `DaemonReady`
//!   to await
//! - a scheduler: `SchedulerHandle::send` buffers until `run()` receives
//! - async work settling: `common::poll::poll_until_async` on the observable
//! - a subprocess: `common::poll::poll_until` on its socket or pid
//! - a mock provider that must block: gate it on a channel the test releases
//! - an mtime that must advance: `File::set_modified`
//!
//! The only sleeps allowed are the ones inside `tests/common/poll.rs`, which
//! are bounded by a deadline and end the moment their condition holds. Any
//! other sleep needs `// sleep-justified: <why no condition exists to wait on>`
//! on the line immediately above it. Expect that line to be questioned.

use std::path::Path;

/// Files whose sleeps are the mechanism, not a guess.
const ALLOWED_FILES: &[&str] = &["common/poll.rs"];
const MARKER: &str = "sleep-justified:";

#[test]
fn no_sleep_in_tests() {
    let tests_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let this_file = Path::new(file!())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let mut violations = Vec::new();
    for entry in walkdir::WalkDir::new(&tests_dir)
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let rel = path
            .strip_prefix(&tests_dir)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if ALLOWED_FILES.contains(&rel.as_str()) || rel == this_file {
            continue;
        }
        let src = std::fs::read_to_string(path).unwrap();
        let lines: Vec<&str> = src.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if !is_sleep_call(line) {
                continue;
            }
            let justified = i > 0 && {
                let above = lines[i - 1].trim_start();
                above.starts_with("//") && above.contains(MARKER)
            };
            if !justified {
                violations.push(format!("tests/{rel}:{}: {}", i + 1, line.trim()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "{} sleep(s) in tests without a `// {MARKER} ...` line immediately above:\n  {}\n\n\
         Wait on the condition instead — the module doc at the top of this file lists what to use.",
        violations.len(),
        violations.join("\n  ")
    );
}

/// True when `line` calls `sleep(` or `sleep_until(` outside a trailing
/// comment: `tokio::time::sleep(`, `thread::sleep(`, bare `sleep(`. A longer
/// identifier ending in `sleep` (`no_sleep(`) does not count.
fn is_sleep_call(line: &str) -> bool {
    let code = line.split("//").next().unwrap_or("");
    ["sleep(", "sleep_until("].iter().any(|needle| {
        code.match_indices(needle).any(|(idx, _)| {
            idx == 0 || {
                let prev = code.as_bytes()[idx - 1];
                !(prev.is_ascii_alphanumeric() || prev == b'_')
            }
        })
    })
}
