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
//! The only sleeps allowed are the ones inside `tests/suite/common/poll.rs`, which
//! are bounded by a deadline and end the moment their condition holds. Any
//! other sleep needs `// sleep-justified: <why no condition exists to wait on>`
//! on the line immediately above it. Expect that line to be questioned.
//!
//! Coverage isn't just `tests/`: unit tests migrated into `src/` and
//! `libbeachcomber/src/` (commit 79677d4 — `*_tests.rs` files, `#[cfg(test)]
//! mod tests { .. }` beside the code they test) are scanned too, but only
//! their test code — a `sleep(` in production code (e.g. `proc_snapshot.rs`'s
//! sampling window, `singleton/mod.rs`'s poll loops) must never be flagged.

use std::path::Path;

/// Files whose sleeps are the mechanism, not a guess.
const ALLOWED_FILES: &[&str] = &["suite/common/poll.rs"];
const MARKER: &str = "sleep-justified:";

/// Crate roots outside `tests/` that carry unit tests beside their production
/// code, relative to this workspace member's manifest directory.
const SRC_ROOTS: &[&str] = &["src", "libbeachcomber/src"];

#[test]
fn no_sleep_in_tests() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let tests_dir = manifest_dir.join("tests");
    let this_file = Path::new(file!())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let mut violations = Vec::new();

    // `tests/` — every `.rs` file here is test code end to end (except the
    // allowlist and this guard's own file), as before.
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
        let rel_file_name = Path::new(&rel).file_name().unwrap().to_string_lossy();
        if ALLOWED_FILES.contains(&rel.as_str()) || rel_file_name == this_file {
            continue;
        }
        let src = std::fs::read_to_string(path).unwrap();
        check_lines(&src, 0, &format!("tests/{rel}"), &mut violations);
    }

    // `src/` and `libbeachcomber/src/` — a file named `*_tests.rs` is test
    // code throughout; any other file is production code except inside its
    // last `#[cfg(test)] mod name { .. }` block, which this codebase's
    // layout keeps terminal (nothing follows it but the block's own close).
    // A bare `#[cfg(test)] mod name;` declaration (an out-of-line module,
    // e.g. `provider/network/mod.rs`'s `mod network_tests;`) opens no block
    // of its own and is skipped here — its target file is scanned directly
    // as a `*_tests.rs` file.
    for root in SRC_ROOTS {
        let dir = manifest_dir.join(root);
        for entry in walkdir::WalkDir::new(&dir)
            .into_iter()
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let rel = path
                .strip_prefix(manifest_dir)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let is_tests_file = path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with("_tests.rs"));
            let src = std::fs::read_to_string(path).unwrap();
            if is_tests_file {
                check_lines(&src, 0, &rel, &mut violations);
            } else if let Some(start) = cfg_test_block_start(&src) {
                check_lines(&src, start, &rel, &mut violations);
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

/// The 0-indexed line at which `src`'s last `#[cfg(test)] mod name { .. }`
/// block starts (the `#[cfg(test)]` line itself), or `None` when the file
/// has no such block. Only a block form (`mod name {`, found by looking a
/// few lines past `#[cfg(test)]` for the `mod` it gates) counts — a
/// declaration form (`mod name;`) doesn't open a scope of its own.
fn cfg_test_block_start(src: &str) -> Option<usize> {
    let lines: Vec<&str> = src.lines().collect();
    let mut start = None;
    for (i, line) in lines.iter().enumerate() {
        if line.trim() != "#[cfg(test)]" {
            continue;
        }
        for next in lines.iter().skip(i + 1).take(4) {
            let t = next.trim();
            if t.starts_with("mod ")
                || t.starts_with("pub mod ")
                || t.starts_with("pub(crate) mod ")
            {
                if t.ends_with('{') {
                    start = Some(i);
                }
                break;
            }
        }
    }
    start
}

/// Scan `src`'s lines from `from_line` (0-indexed) to the end, recording an
/// unjustified `sleep(`/`sleep_until(` call under `label:<line>:`.
fn check_lines(src: &str, from_line: usize, label: &str, violations: &mut Vec<String>) {
    let lines: Vec<&str> = src.lines().collect();
    for (i, line) in lines.iter().enumerate().skip(from_line) {
        if !is_sleep_call(line) {
            continue;
        }
        let justified = i > 0 && {
            let above = lines[i - 1].trim_start();
            above.starts_with("//") && above.contains(MARKER)
        };
        if !justified {
            violations.push(format!("{label}:{}: {}", i + 1, line.trim()));
        }
    }
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
