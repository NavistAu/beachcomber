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
    /// Git user identity is set to `test@test.com` / `Test` so commits work in
    /// environments without a global git config.
    pub fn new() -> Self {
        if !has_git() {
            panic!(
                "git binary not found — call has_git() and skip the test before constructing GitRepoFixture"
            );
        }
        let dir = TempDir::new().expect("TempDir::new");
        let path = dir.path();

        git(path, &["init"]);
        git(path, &["config", "user.email", "test@test.com"]);
        git(path, &["config", "user.name", "Test"]);
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
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap_or_else(|e| panic!("git {:?}: {e}", args));
        // non-zero exit is intentionally not checked — caller expects it
    }
}

// ---------------------------------------------------------------------------
// Public helper
// ---------------------------------------------------------------------------

/// Returns `true` if the `git` binary is available on `$PATH`.
/// Tests that depend on real git should call this and skip cleanly if false.
#[allow(dead_code)]
pub fn has_git() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_ok()
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

#[allow(dead_code)]
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
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
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
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
