/// Seam tests for the `git` plugin.
///
/// Uses a hand-written `StubGitExecutor` to provide canned git command outputs
/// without requiring git to be installed or a real repository on disk.
///
/// Coverage:
/// - clean repo: refs source produces correct commit fields
/// - dirty repo: status source detects staged/unstaged/untracked counts
/// - detached HEAD: refs source asks about no push remote
/// - no repo (all git commands fail): sources fail or answer nothing
use super::{diff_facts, lineage_facts, refs_facts, status_facts};
use crate::boundaries::git::GitExecutor;
use serde_json::json;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{ExitStatus, Output};
use std::sync::Mutex;
use tempfile::TempDir;

// ── Hand-written test double ──────────────────────────────────────────────────

/// A `GitExecutor` that answers each command from a list by its leading words
/// (a symbol starts some of its commands together, so order says nothing): the
/// first entry the command's arguments start with, taken once. With none left,
/// a "command not found" IO error.
struct StubGitExecutor {
    outputs: Mutex<Vec<(&'static str, std::io::Result<Output>)>>,
}

impl StubGitExecutor {
    fn new(outputs: Vec<(&'static str, std::io::Result<Output>)>) -> Self {
        Self {
            outputs: Mutex::new(outputs),
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
    fn run_git(&self, _dir: &Path, args: Vec<String>) -> std::io::Result<Output> {
        let command = args.join(" ");
        let mut outputs = self.outputs.lock().unwrap();
        match outputs.iter().position(|(c, _)| command.starts_with(c)) {
            Some(i) => outputs.remove(i).1,
            None => not_found(),
        }
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

/// The refs source starts these together, and asks about nothing else with no
/// push remote configured.
fn refs_outputs_clean() -> Vec<(&'static str, std::io::Result<Output>)> {
    vec![
        ("log -1", success(log_output())),
        ("describe", failure()), // no tags
        ("config --list", success(b"user.name\nme\0")),
    ]
}

#[test]
fn git_refs_clean_repo_produces_correct_commit() {
    let dir = fake_repo();
    let stub = StubGitExecutor::new(refs_outputs_clean());
    let result = refs_facts(dir.path(), &stub).expect("facts");

    // The head source's and the status source's are not said here.
    for other in ["branch", "detached", "ahead", "behind", "upstream", "dirty"] {
        assert!(!result.contains_key(other), "{other}: {result:?}");
    }
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
fn git_refs_without_commits_leaves_them_absent() {
    let dir = fake_repo();
    let stub = StubGitExecutor::new(vec![
        ("log -1", failure()), // no commits yet
        ("describe", failure()),
        ("config --list", failure()),
        ("rev-parse --git-dir", success(b".git\n")),
    ]);
    let result = refs_facts(dir.path(), &stub).expect("facts");

    for absent in ["commit", "commit_summary", "last_commit_age_secs"] {
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
fn git_refs_detached_head_asks_for_no_push_remote() {
    let dir = fake_repo();
    std::fs::write(
        dir.path().join(".git/HEAD"),
        "abc1234def5678901234567890123456789abcde\n",
    )
    .unwrap();
    let stub = StubGitExecutor::new(vec![
        ("log -1", success(log_output())),
        ("describe", failure()),
    ]);
    let result = refs_facts(dir.path(), &stub).expect("facts");

    assert!(!result.contains_key("push_ahead"), "{:?}", result);
    assert!(!result.contains_key("push_behind"), "{:?}", result);
}

/// The last value of each key stands, and a branch's own push remote beats the
/// default, as `git config --get` has it.
#[test]
fn git_refs_push_remote_from_the_one_listing() {
    let dir = fake_repo();
    std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let listing: &[u8] = b"remote.pushdefault\nfirst\0remote.pushdefault\nsecond\0\
        branch.other.pushremote\nnot-this\0novalue\0";
    let stub = StubGitExecutor::new(vec![
        ("log -1", success(log_output())),
        ("describe", failure()),
        ("config --list", success(listing)),
        (
            "rev-parse --verify --quiet refs/remotes/second/main",
            success(b"abc\n"),
        ),
        (
            "rev-list --count --left-right HEAD...second/main",
            success(b"2\t3\n"),
        ),
    ]);
    let result = refs_facts(dir.path(), &stub).expect("facts");
    assert_eq!(result.get("push_ahead"), Some(&json!(2)));
    assert_eq!(result.get("push_behind"), Some(&json!(3)));

    let listing: &[u8] = b"remote.pushdefault\nsecond\0branch.main.pushremote\nmine\0";
    let stub = StubGitExecutor::new(vec![
        ("log -1", success(log_output())),
        ("describe", failure()),
        ("config --list", success(listing)),
        (
            "rev-parse --verify --quiet refs/remotes/mine/main",
            success(b"abc\n"),
        ),
        (
            "rev-list --count --left-right HEAD...mine/main",
            success(b"1\t0\n"),
        ),
    ]);
    let result = refs_facts(dir.path(), &stub).expect("facts");
    assert_eq!(result.get("push_ahead"), Some(&json!(1)));
}

#[test]
fn git_refs_git_not_installed_fails() {
    let dir = fake_repo();
    let stub = StubGitExecutor::new(vec![]);
    assert!(refs_facts(dir.path(), &stub).is_none());
}

#[test]
fn git_refs_a_repository_git_cannot_read_fails() {
    let dir = fake_repo();
    let stub = StubGitExecutor::new(vec![
        ("log -1", failure()),
        ("describe", failure()),
        ("rev-parse --git-dir", failure()),
    ]);
    assert!(refs_facts(dir.path(), &stub).is_none());
}

// ── lineage source seam tests ─────────────────────────────────────────────────

/// What two commits differ by is asked of git once: a run after a ref was
/// rewritten with what it held spawns only the `merge-base`. Another commit is
/// another comparison.
#[test]
fn git_lineage_compares_two_commits_once() {
    let dir = fake_repo();
    let git = dir.path().join(".git");
    std::fs::write(git.join("HEAD"), "ref: refs/heads/topic\n").unwrap();
    std::fs::create_dir_all(git.join("refs/heads")).unwrap();
    std::fs::write(git.join("refs/heads/main"), "").unwrap();
    let at = |hex: &str| {
        std::fs::write(
            git.join("refs/heads/topic"),
            format!("{}\n", hex.repeat(40)),
        )
        .unwrap();
    };
    let point = "merge-base --fork-point main HEAD";
    let counted = |numstat: &'static [u8]| {
        let stub = StubGitExecutor::new(vec![
            (point, success(b"p0int\n")),
            ("diff --numstat p0int..HEAD", success(numstat)),
        ]);
        let facts = lineage_facts(dir.path(), &stub).expect("facts");
        facts.get("lines_added_default").cloned().unwrap()
    };
    at("a");
    assert_eq!(counted(b"3\t1\tf\n"), json!(3));
    assert_eq!(counted(b"9\t9\tf\n"), json!(3), "the same two commits");
    at("b");
    assert_eq!(counted(b"9\t9\tf\n"), json!(9), "another commit");
}

/// A comparison that failed says nothing about the two commits: it is not kept.
#[test]
fn git_lineage_keeps_no_failed_comparison() {
    let dir = fake_repo();
    let git = dir.path().join(".git");
    std::fs::write(git.join("HEAD"), "ref: refs/heads/topic\n").unwrap();
    std::fs::create_dir_all(git.join("refs/heads")).unwrap();
    std::fs::write(git.join("refs/heads/main"), "").unwrap();
    std::fs::write(
        git.join("refs/heads/topic"),
        format!("{}\n", "c".repeat(40)),
    )
    .unwrap();
    let point = "merge-base --fork-point main HEAD";
    let stub = StubGitExecutor::new(vec![(point, success(b"p0int\n"))]);
    lineage_facts(dir.path(), &stub).expect("facts");
    let stub = StubGitExecutor::new(vec![
        (point, success(b"p0int\n")),
        ("diff --numstat p0int..HEAD", success(b"5\t0\tf\n")),
    ]);
    let facts = lineage_facts(dir.path(), &stub).expect("facts");
    assert_eq!(facts.get("lines_added_default"), Some(&json!(5)));
}

// ── status source seam tests ──────────────────────────────────────────────────

#[test]
fn git_status_clean_repo_is_not_dirty() {
    let dir = fake_repo();
    let stub = StubGitExecutor::new(vec![("status", success(status_clean_main()))]);
    let result = status_facts(dir.path(), &stub).expect("facts");

    assert_eq!(result.get("dirty").unwrap(), &json!(false));
    assert_eq!(result.get("staged").unwrap(), &json!(0));
    assert_eq!(result.get("unstaged").unwrap(), &json!(0));
    assert_eq!(result.get("untracked").unwrap(), &json!(0));
    assert_eq!(result.get("conflicted").unwrap(), &json!(0));
    assert_eq!(result.get("ahead").unwrap(), &json!(0));
    assert_eq!(result.get("behind").unwrap(), &json!(0));
    assert_eq!(
        result.get("upstream").unwrap().as_str().unwrap(),
        "origin/main"
    );
}

#[test]
fn git_status_without_an_upstream_leaves_it_absent() {
    let dir = fake_repo();
    // No branch.upstream, but a branch.ab line.
    let stub = StubGitExecutor::new(vec![("status", success(status_one_staged()))]);
    let result = status_facts(dir.path(), &stub).expect("facts");
    for absent in ["upstream", "ahead", "behind"] {
        assert!(!result.contains_key(absent), "{absent}: {result:?}");
    }
}

#[test]
fn git_status_untracked_file_is_dirty() {
    let dir = fake_repo();
    let stub = StubGitExecutor::new(vec![("status", success(status_dirty_untracked()))]);
    let result = status_facts(dir.path(), &stub).expect("facts");

    assert_eq!(result.get("dirty").unwrap(), &json!(true));
    assert_eq!(result.get("untracked").unwrap(), &json!(1));
    assert_eq!(result.get("staged").unwrap(), &json!(0));
}

#[test]
fn git_status_staged_file_is_dirty() {
    let dir = fake_repo();
    let stub = StubGitExecutor::new(vec![("status", success(status_one_staged()))]);
    let result = status_facts(dir.path(), &stub).expect("facts");

    assert_eq!(result.get("dirty").unwrap(), &json!(true));
    assert_eq!(result.get("staged").unwrap(), &json!(1));
    assert_eq!(result.get("unstaged").unwrap(), &json!(0));
}

#[test]
fn git_status_git_not_installed_fails() {
    let dir = fake_repo();
    let stub = StubGitExecutor::new(vec![]);
    assert!(status_facts(dir.path(), &stub).is_none());
}

// ── diff source seam tests ────────────────────────────────────────────────────

#[test]
fn git_diff_clean_repo_has_zero_lines() {
    let dir = fake_repo();
    let stub = StubGitExecutor::new(vec![
        ("diff --numstat", success(diff_empty())),
        ("diff --cached --numstat", success(diff_empty())),
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
        ("diff --numstat", success(diff_3added_1removed())),
        ("diff --cached --numstat", success(diff_empty())),
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
        (
            "diff --numstat",
            success(b"-\t-\tsome_binary.bin\n3\t1\tREADME.md\n"),
        ),
        ("diff --cached --numstat", success(diff_empty())),
    ]);
    let result = diff_facts(dir.path(), &stub).expect("facts");

    // Binary file "-" parses as 0 via parse::<i64>().unwrap_or(0)
    assert_eq!(result.get("lines_added").unwrap(), &json!(3));
    assert_eq!(result.get("lines_removed").unwrap(), &json!(1));
}

#[test]
fn git_diff_git_error_returns_zero_counts() {
    let dir = fake_repo();
    let stub = StubGitExecutor::new(vec![]);
    let result = diff_facts(dir.path(), &stub).expect("facts");

    // Errors produce (0,0) for both unstaged and staged
    assert_eq!(result.get("lines_added").unwrap(), &json!(0));
    assert_eq!(result.get("lines_removed").unwrap(), &json!(0));
}
