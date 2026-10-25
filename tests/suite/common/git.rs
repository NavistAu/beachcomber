/// Shared git repository fixture for integration tests.
///
/// Provides helpers to create temporary git repos in common states (clean,
/// dirty, staged, detached HEAD, etc.) without duplicating setup boilerplate
/// across test files.
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

/// A temporary git repository with standard test identity configured.
///
/// The underlying `TempDir` is kept alive for the lifetime of the fixture.
/// Drop order: callers must not use `path()` after the fixture is dropped.
#[allow(dead_code)]
pub struct GitRepoFixture {
    pub dir: TempDir,
}

#[allow(dead_code)]
impl GitRepoFixture {
    /// Create a clean repository with one initial commit (`"init"`, adds `README.md`).
    ///
    /// Its commits are by `Test <test@test.com>` ([`git_cmd`] says so), so
    /// they work in environments without a global git config. A machine with
    /// no git fails the test that wanted one, loudly.
    pub fn new() -> Self {
        assert!(
            has_git(),
            "git binary not found: this test needs git on $PATH"
        );
        let dir = TempDir::new().expect("TempDir::new");
        let path = dir.path();

        git(path, &["init", "--initial-branch=main"]);
        std::fs::write(path.join("README.md"), "# test").expect("write README.md");
        git(path, &["add", "."]);
        git(path, &["commit", "-m", "init"]);

        Self { dir }
    }

    /// Absolute path to the repository root.
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Absolute path as a `String` (convenience for `Source::execute`).
    pub fn path_str(&self) -> &str {
        self.dir.path().to_str().expect("path is valid UTF-8")
    }

    /// Add an untracked file to make the repo dirty.
    ///
    /// Returns `self` for chaining.
    pub fn with_untracked_file(self, name: &str, content: &str) -> Self {
        std::fs::write(self.path().join(name), content)
            .unwrap_or_else(|e| panic!("write {name}: {e}"));
        self
    }

    /// Modify an existing tracked file (unstaged change).
    pub fn with_unstaged_change(self, name: &str, content: &str) -> Self {
        std::fs::write(self.path().join(name), content)
            .unwrap_or_else(|e| panic!("write {name}: {e}"));
        self
    }

    /// Stage a new file.
    pub fn with_staged_file(self, name: &str, content: &str) -> Self {
        std::fs::write(self.path().join(name), content)
            .unwrap_or_else(|e| panic!("write {name}: {e}"));
        git(self.path(), &["add", name]);
        self
    }

    /// Stash all current changes (requires at least one dirty file already written).
    pub fn with_stash(self) -> Self {
        git(self.path(), &["stash"]);
        self
    }

    /// Detach HEAD at the current commit.
    pub fn with_detached_head(self) -> Self {
        let sha = git_output(self.path(), &["rev-parse", "HEAD"]);
        let sha = sha.trim();
        git(self.path(), &["checkout", "--detach", sha]);
        self
    }

    /// Create and check out a new branch.
    pub fn with_branch(self, name: &str) -> Self {
        git(self.path(), &["checkout", "-b", name]);
        self
    }

    /// Create and check out a new branch from an explicit start point, so the
    /// branch's reflog records `branch: Created from <start>` rather than `HEAD`.
    pub fn with_branch_from(self, name: &str, start: &str) -> Self {
        git(self.path(), &["checkout", "-b", name, start]);
        self
    }

    /// Add an additional commit with a given message (commits all staged changes).
    pub fn with_commit(self, message: &str) -> Self {
        git(self.path(), &["commit", "--allow-empty", "-m", message]);
        self
    }

    /// Create a subdirectory inside the repo (returned as an absolute `PathBuf`).
    pub fn create_subdir(&self, rel: &str) -> PathBuf {
        let p = self.path().join(rel);
        std::fs::create_dir_all(&p).unwrap_or_else(|e| panic!("create_dir_all {rel}: {e}"));
        p
    }

    /// Create a linked worktree checked out on a new branch `branch`, located at
    /// `<repo>/<name>`. Returns its absolute path. `<name>/.git` will be a *file*.
    pub fn add_worktree(&self, name: &str, branch: &str) -> PathBuf {
        git(self.path(), &["worktree", "add", name, "-b", branch]);
        self.path().join(name)
    }

    /// Run a git command in an arbitrary directory (e.g. a worktree root).
    pub fn git_in(&self, dir: &Path, args: &[&str]) {
        git(dir, args);
    }

    /// Like `git_in` but does NOT panic on non-zero exit. Use when the command
    /// is expected to "fail" (e.g. `git merge` that produces a conflict).
    pub fn git_in_allow_failure(&self, dir: &Path, args: &[&str]) {
        git_cmd(dir, args)
            .output()
            .unwrap_or_else(|e| panic!("git {:?}: {e}", args));
        // non-zero exit is intentionally not checked — caller expects it
    }
}

// ---------------------------------------------------------------------------
// Public helper
// ---------------------------------------------------------------------------

/// Returns `true` if the `git` binary is available on `$PATH`, asked once
/// per test process.
#[allow(dead_code)]
pub fn has_git() -> bool {
    static HAS_GIT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HAS_GIT.get_or_init(|| {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_ok()
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// The variables that point git at a repository other than the one
/// `current_dir` is in. A test run under `git rebase --exec` inherits
/// `GIT_DIR`, and a fixture would then write its config and commits into the
/// repository being rebased.
const REPO_VARS: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

/// `git` in `dir` and nowhere else, with the host's configuration shut out and
/// the test identity, `Test <test@test.com>`, as the author and committer of
/// anything it commits. A `commit.gpgsign = true` in the developer's global
/// config turns every fixture commit into a signing round-trip — ~100ms each,
/// serialised through the agent when tests run in parallel — and
/// `init.defaultBranch` would make the initial branch name depend on the
/// machine. The identity rides in the environment rather than a `git config`
/// per repository, which was two more processes for every fixture.
pub fn git_cmd(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com");
    for var in REPO_VARS {
        cmd.env_remove(var);
    }
    cmd
}

/// [`git_cmd`], run, panicking with git's stderr if it fails.
#[allow(dead_code)]
pub fn git(dir: &Path, args: &[&str]) {
    let out = git_cmd(dir, args)
        .output()
        .unwrap_or_else(|e| panic!("git {:?}: {e}", args));
    if !out.status.success() {
        panic!(
            "git {:?} failed ({}): {}",
            args,
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[allow(dead_code)]
fn git_output(dir: &Path, args: &[&str]) -> String {
    let out = git_cmd(dir, args)
        .output()
        .unwrap_or_else(|e| panic!("git {:?}: {e}", args));
    if !out.status.success() {
        panic!(
            "git {:?} failed ({}): {}",
            args,
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    String::from_utf8(out.stdout).expect("git output is UTF-8")
}

/// A fixture builds in its own directory even when the test inherits a
/// `GIT_DIR`, as it does under `git rebase --exec`: the repository `GIT_DIR`
/// names gets neither the fixture's identity nor its commit.
#[test]
fn a_fixture_ignores_an_inherited_git_dir() {
    let decoy = TempDir::new().expect("TempDir::new");
    git(decoy.path(), &["init", "--initial-branch=main"]);
    // SAFETY: nextest runs each test in its own process, so nothing else
    // reads this environment.
    unsafe { std::env::set_var("GIT_DIR", decoy.path().join(".git")) };

    let fixture = GitRepoFixture::new();

    assert!(
        !decoy.path().join(".git/refs/heads/main").exists(),
        "the decoy got the fixture's commit"
    );
    assert!(fixture.path().join(".git/refs/heads/main").exists());
}
