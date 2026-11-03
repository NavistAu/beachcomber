/// Seam tests for the `git` plugin.
///
/// Uses a hand-written `StubGitExecutor` to provide canned git command outputs
/// without requiring git to be installed or a real repository on disk.
///
/// Coverage:
/// - clean repo: refs source produces correct branch/commit fields
/// - dirty repo: status source detects staged/unstaged/untracked counts
/// - detached HEAD: refs source sets detached=true
/// - no repo (all git commands fail): sources return empty result
/// - malformed output: provider handles gracefully without panicking
use super::{diff_facts, refs_facts, status_facts};
use crate::boundaries::git::GitExecutor;
use serde_json::json;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{ExitStatus, Output};
use std::sync::Mutex;
use tempfile::TempDir;

// ── Hand-written test double ──────────────────────────────────────────────────

/// A `GitExecutor` that returns a pre-programmed sequence of `Output` values.
/// Each call to `run_git` pops from the front of the queue.
/// If the queue is empty, returns a "command not found" IO error.
struct StubGitExecutor {
    outputs: Mutex<std::collections::VecDeque<std::io::Result<Output>>>,
}

impl StubGitExecutor {
    fn new(outputs: Vec<std::io::Result<Output>>) -> Self {
        Self {
            outputs: Mutex::new(outputs.into()),
        }
    }
}

fn success(stdout: &[u8]) -> std::io::Result<Output> {
    Ok(Output {
        status: ExitStatus::from_raw(0),
        stdout: stdout.to_vec(),
        stderr: vec![],
    })
}

fn failure() -> std::io::Result<Output> {
    Ok(Output {
        status: ExitStatus::from_raw(1 << 8),
        stdout: vec![],
        stderr: vec![],
    })
}

fn not_found() -> std::io::Result<Output> {
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "git: command not found",
    ))
}

impl GitExecutor for StubGitExecutor {
    fn run_git(&self, _dir: &Path, _args: Vec<String>) -> std::io::Result<Output> {
        self.outputs
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(not_found)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Create a temporary directory to use as a fake repo path.
///
/// The stub executor never looks at the filesystem, but every git Source
/// refuses to answer for a directory that is not a work-tree root — a Source
/// reporting defaults for a directory it knows nothing about turns a miss into
/// a false hit. An empty `.git` directory is enough to satisfy that guard while
/// leaving the stubbed command output as the only thing under test.
fn fake_repo() -> TempDir {
    let dir = TempDir::new().expect("TempDir::new");
    std::fs::create_dir(dir.path().join(".git")).expect("create .git");
    dir
}

/// `git status --porcelain=v2 --branch` output for a clean repo on `main`.
fn status_clean_main() -> &'static [u8] {
    b"# branch.oid abc1234def5678901234567890123456789abcde\n\
      # branch.head main\n\
      # branch.upstream origin/main\n\
      # branch.ab +0 -0\n"
}

/// `git status --porcelain=v2 --branch` output for a dirty repo (1 untracked).
fn status_dirty_untracked() -> &'static [u8] {
    b"# branch.oid abc1234def5678901234567890123456789abcde\n\
      # branch.head main\n\
      # branch.upstream origin/main\n\
      # branch.ab +0 -0\n\
      ? new_file.txt\n"
}

/// `git status --porcelain=v2 --branch` output for a repo with 1 staged file.
fn status_one_staged() -> &'static [u8] {
    b"# branch.oid abc1234def5678901234567890123456789abcde\n\
      # branch.head main\n\
      # branch.ab +0 -0\n\
      1 A. N... 0 0 0 0000000 abc1234 staged.txt\n"
}

/// `git status --porcelain=v2 --branch` output for a detached HEAD.
fn status_detached() -> &'static [u8] {
    b"# branch.oid abc1234def5678901234567890123456789abcde\n\
      # branch.head (detached)\n"
}

/// `git log -1 --format=%h %ct %s` output.
fn log_output() -> &'static [u8] {
    b"abc1234 1700000000 init\n"
}

/// `git diff --numstat` output (no changes).
fn diff_empty() -> &'static [u8] {
    b""
}

/// `git diff --numstat` output (3 added, 1 removed in one file).
fn diff_3added_1removed() -> &'static [u8] {
    b"3\t1\tREADME.md\n"
}

// ── refs source seam tests ────────────────────────────────────────────────────

/// For the refs source, `execute` makes these calls in order:
///   1. `git status --porcelain=v2 --branch`
///   2. `git log -1 --format=%h %ct %s`
///   3. `git describe --tags --abbrev=0`
///   4. `git config --get branch.<name>.pushRemote`  (returns non-zero → skip)
///   5. `git config --get remote.pushDefault`         (returns non-zero → skip)
fn refs_outputs_clean() -> Vec<std::io::Result<Output>> {
    vec![
        success(status_clean_main()), // status --porcelain=v2 --branch
        success(log_output()),        // log -1 --format=...
        failure(),                    // describe --tags (no tags)
        failure(),                    // config --get branch.main.pushRemote
        failure(),                    // config --get remote.pushDefault
    ]
}

#[test]
fn git_refs_clean_repo_produces_correct_branch() {
    // Note: branch and detached are the head source's.
    // The refs source now produces commit, ahead, behind, upstream, state, etc.
    let dir = fake_repo();
    let stub = StubGitExecutor::new(refs_outputs_clean());
    let result = refs_facts(dir.path(), &stub).expect("facts");

    assert!(
        !result.contains_key("branch"),
        "branch moved to head source; must not be present in refs"
    );
    assert!(
        !result.contains_key("detached"),
        "detached moved to head source; must not be present in refs"
    );
    assert_eq!(result.get("ahead").unwrap(), &json!(0));
    assert_eq!(result.get("behind").unwrap(), &json!(0));
    assert_eq!(
        result.get("upstream").unwrap().as_str().unwrap(),
        "origin/main"
    );
    assert_eq!(result.get("commit").unwrap().as_str().unwrap(), "abc1234");
    assert_eq!(
        result.get("commit_summary").unwrap().as_str().unwrap(),
        "init"
    );
    // What the repo does not have is absent, not "" or 0: no tag, no push
    // remote, and no rebase or other stepped operation in progress.
    for absent in [
        "tag",
        "push_ahead",
        "push_behind",
        "state_step",
        "state_total",
    ] {
        assert!(
            !result.contains_key(absent),
            "{absent} must be absent: {:?}",
            result
        );
    }
    assert_eq!(result.get("state").unwrap().as_str().unwrap(), "clean");
}

#[test]
fn git_refs_without_an_upstream_or_commits_leaves_them_absent() {
    let dir = fake_repo();
    let stub = StubGitExecutor::new(vec![
        success(status_one_staged()), // no branch.upstream, but a branch.ab line
        failure(),                    // log -1: no commits yet
        failure(),                    // describe --tags
        failure(),                    // config --get branch.main.pushRemote
        failure(),                    // config --get remote.pushDefault
    ]);
    let result = refs_facts(dir.path(), &stub).expect("facts");

    for absent in [
        "upstream",
        "ahead",
        "behind",
        "commit",
        "commit_summary",
        "last_commit_age_secs",
    ] {
        assert!(
            !result.contains_key(absent),
            "{absent} must be absent: {:?}",
            result
        );
    }
    // A count is a count: zero stashes is a value.
    assert_eq!(result.get("stash"), Some(&json!(0)));
}

#[test]
fn git_refs_detached_head_sets_detached_true() {
    // detached is now owned by the head source (reads .git/HEAD directly).
    // The refs source no longer emits detached, and has no push divergence
    // to report in the detached case.
    let dir = fake_repo();
    let stub = StubGitExecutor::new(vec![
        success(status_detached()), // status
        success(log_output()),      // log
        failure(),                  // describe
                                    // no push remote queries since branch is "(detached)"
    ]);
    let result = refs_facts(dir.path(), &stub).expect("facts");

    assert!(
        !result.contains_key("detached"),
        "detached moved to head source; must not be present in refs"
    );
    assert!(!result.contains_key("push_ahead"), "{:?}", result);
    assert!(!result.contains_key("push_behind"), "{:?}", result);
}

#[test]
fn git_refs_git_not_installed_fails() {
    let dir = fake_repo();
    let stub = StubGitExecutor::new(vec![not_found()]);
    assert!(refs_facts(dir.path(), &stub).is_none());
}

#[test]
fn git_refs_git_returns_failure_exit_code_fails() {
    let dir = fake_repo();
    // status exits non-zero → parse_git_status returns None → the execute fails
    let stub = StubGitExecutor::new(vec![failure()]);
    assert!(refs_facts(dir.path(), &stub).is_none());
}

#[test]
fn git_refs_malformed_status_output_returns_empty() {
    let dir = fake_repo();
    // success exit code but no parseable branch header → parse_git_status returns
    // a struct with empty branch (which is non-None), so we still get a result.
    // The important thing: no panic.
    let stub = StubGitExecutor::new(vec![
        success(b"this is not valid git status output\n"),
        success(log_output()),
        failure(), // describe
        failure(), // config pushRemote (branch "" → skip immediately)
    ]);
    // Must not panic
    let _result = refs_facts(dir.path(), &stub).expect("facts");
}

// ── status source seam tests ──────────────────────────────────────────────────

#[test]
fn git_status_clean_repo_is_not_dirty() {
    let dir = fake_repo();
    let stub = StubGitExecutor::new(vec![success(status_clean_main())]);
    let result = status_facts(dir.path(), &stub).expect("facts");

    assert_eq!(result.get("dirty").unwrap(), &json!(false));
    assert_eq!(result.get("staged").unwrap(), &json!(0));
    assert_eq!(result.get("unstaged").unwrap(), &json!(0));
    assert_eq!(result.get("untracked").unwrap(), &json!(0));
    assert_eq!(result.get("conflicted").unwrap(), &json!(0));
}

#[test]
fn git_status_untracked_file_is_dirty() {
    let dir = fake_repo();
    let stub = StubGitExecutor::new(vec![success(status_dirty_untracked())]);
    let result = status_facts(dir.path(), &stub).expect("facts");

    assert_eq!(result.get("dirty").unwrap(), &json!(true));
    assert_eq!(result.get("untracked").unwrap(), &json!(1));
    assert_eq!(result.get("staged").unwrap(), &json!(0));
}

#[test]
fn git_status_staged_file_is_dirty() {
    let dir = fake_repo();
    let stub = StubGitExecutor::new(vec![success(status_one_staged())]);
    let result = status_facts(dir.path(), &stub).expect("facts");

    assert_eq!(result.get("dirty").unwrap(), &json!(true));
    assert_eq!(result.get("staged").unwrap(), &json!(1));
    assert_eq!(result.get("unstaged").unwrap(), &json!(0));
}

#[test]
fn git_status_git_not_installed_fails() {
    let dir = fake_repo();
    let stub = StubGitExecutor::new(vec![not_found()]);
    assert!(status_facts(dir.path(), &stub).is_none());
}

// ── diff source seam tests ────────────────────────────────────────────────────

/// For the diff source, `execute` makes these two calls:
///   1. `git diff --numstat`
///   2. `git diff --cached --numstat`
#[test]
fn git_diff_clean_repo_has_zero_lines() {
    let dir = fake_repo();
    let stub = StubGitExecutor::new(vec![
        success(diff_empty()), // diff --numstat
        success(diff_empty()), // diff --cached --numstat
    ]);
    let result = diff_facts(dir.path(), &stub).expect("facts");

    assert_eq!(result.get("lines_added").unwrap(), &json!(0));
    assert_eq!(result.get("lines_removed").unwrap(), &json!(0));
    assert_eq!(result.get("lines_staged_added").unwrap(), &json!(0));
    assert_eq!(result.get("lines_staged_removed").unwrap(), &json!(0));
}

#[test]
fn git_diff_counts_unstaged_lines() {
    let dir = fake_repo();
    let stub = StubGitExecutor::new(vec![
        success(diff_3added_1removed()), // diff --numstat
        success(diff_empty()),           // diff --cached --numstat
    ]);
    let result = diff_facts(dir.path(), &stub).expect("facts");

    assert_eq!(result.get("lines_added").unwrap(), &json!(3));
    assert_eq!(result.get("lines_removed").unwrap(), &json!(1));
    assert_eq!(result.get("lines_staged_added").unwrap(), &json!(0));
    assert_eq!(result.get("lines_staged_removed").unwrap(), &json!(0));
}

#[test]
fn git_diff_binary_files_are_skipped_gracefully() {
    // Binary files produce "-\t-\tfile.bin" in numstat output
    let dir = fake_repo();
    let stub = StubGitExecutor::new(vec![
        success(b"-\t-\tsome_binary.bin\n3\t1\tREADME.md\n"), // diff --numstat
        success(diff_empty()),                                // diff --cached --numstat
    ]);
    let result = diff_facts(dir.path(), &stub).expect("facts");

    // Binary file "-" parses as 0 via parse::<i64>().unwrap_or(0)
    assert_eq!(result.get("lines_added").unwrap(), &json!(3));
    assert_eq!(result.get("lines_removed").unwrap(), &json!(1));
}

#[test]
fn git_diff_git_error_returns_zero_counts() {
    let dir = fake_repo();
    let stub = StubGitExecutor::new(vec![
        not_found(), // diff --numstat
        not_found(), // diff --cached --numstat
    ]);
    let result = diff_facts(dir.path(), &stub).expect("facts");

    // Errors produce (0,0) for both unstaged and staged
    assert_eq!(result.get("lines_added").unwrap(), &json!(0));
    assert_eq!(result.get("lines_removed").unwrap(), &json!(0));
}
