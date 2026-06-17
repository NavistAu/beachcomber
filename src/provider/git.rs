use crate::boundaries::git::{GitExecutor, RealGitExecutor};
use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, KeepAlive, Provider,
    ProviderMetadata, Source, SourceMetadata, SourceResult, SourceScope, Value,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Walk upwards from `start` looking for a directory that contains `.git`.
/// Returns the containing directory's absolute path, or `None` if no repo
/// root is found before reaching the filesystem root.
///
/// Uses file-system traversal rather than shelling out to `git rev-parse` so
/// canonicalization stays cheap for the common case (every demand path).
fn walk_to_git(path: Option<&str>) -> Option<String> {
    let p = path?;
    find_repo_root(Path::new(p))
}

fn find_repo_root(start: &Path) -> Option<String> {
    let mut cur: Option<&Path> = Some(start);
    while let Some(dir) = cur {
        if dir.join(".git").exists() {
            return Some(dir.to_string_lossy().to_string());
        }
        cur = dir.parent();
    }
    None
}

/// Resolved git directory locations for a working-tree root.
///
/// For a normal checkout `gitdir == commondir == <root>/.git`. For a linked
/// worktree (`git worktree add`) or a submodule, `<root>/.git` is a *file*
/// pointing at the real per-worktree gitdir; shared state (refs, packed-refs,
/// stash reflog) lives under `commondir`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GitDirs {
    /// Per-worktree git dir: holds HEAD, index, MERGE_HEAD, rebase-merge/, …
    gitdir: PathBuf,
    /// Shared common dir: holds refs/, packed-refs, logs/refs/stash, objects/.
    commondir: PathBuf,
}

/// Resolve the gitdir/commondir for a working-tree root, following the
/// `.git`-is-a-file indirection used by linked worktrees and submodules.
/// Returns `None` if `<root>/.git` does not exist.
fn resolve_git_dir(root: &Path) -> Option<GitDirs> {
    let dot_git = root.join(".git");
    // Use `metadata` (follows symlinks) so a `.git` that is a symlink to a
    // directory correctly reports `is_dir() == true`. A `.git` file (worktree
    // pointer) still gives `is_dir() == false` because it is a regular file.
    let meta = std::fs::metadata(&dot_git).ok()?;

    if meta.is_dir() {
        return Some(GitDirs {
            gitdir: dot_git.clone(),
            commondir: dot_git,
        });
    }

    // `.git` is a file whose first line is `gitdir: <path>` — absolute for
    // worktrees, relative for submodules.
    let contents = std::fs::read_to_string(&dot_git).ok()?;
    let rel = contents.lines().next()?.strip_prefix("gitdir:")?.trim();
    let gitdir = resolve_against(root, rel);

    // `<gitdir>/commondir`, if present, points at the shared common dir (usually
    // relative to the gitdir, e.g. "../.."). Absent for submodules → fall back.
    let commondir = match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(s) => resolve_against(&gitdir, s.trim()),
        Err(_) => gitdir.clone(),
    };

    Some(GitDirs { gitdir, commondir })
}

/// Join `raw` onto `base` unless `raw` is absolute, then normalise via
/// `canonicalize` (falling back to the lexical join if it cannot be canonicalised).
fn resolve_against(base: &Path, raw: &str) -> PathBuf {
    let p = Path::new(raw);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    };
    std::fs::canonicalize(&joined).unwrap_or(joined)
}

// ── SourceMetadata constructors ───────────────────────────────────────────────

fn refs_meta() -> SourceMetadata {
    SourceMetadata {
        name: "refs".into(),
        fields: vec![
            FieldSchema {
                name: "branch".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "commit".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "tag".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "ahead".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "behind".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "upstream".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "detached".into(),
                field_type: FieldType::Bool,
            },
            FieldSchema {
                name: "state".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "stash".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "state_step".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "state_total".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "last_commit_age_secs".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "commit_summary".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "push_ahead".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "push_behind".into(),
                field_type: FieldType::Int,
            },
        ],
        scope: SourceScope::PathScoped,
        invalidation: InvalidationStrategy::Watch {
            patterns: vec![".git".into()],
            abs_paths: vec![],
        },
        keep_alive: KeepAlive::Duration(120),
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 60,
        },
        fsevents_reinstate: true,
    }
}

fn diff_meta() -> SourceMetadata {
    SourceMetadata {
        name: "diff".into(),
        fields: vec![
            FieldSchema {
                name: "lines_added".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "lines_removed".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "lines_staged_added".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "lines_staged_removed".into(),
                field_type: FieldType::Int,
            },
        ],
        scope: SourceScope::PathScoped,
        invalidation: InvalidationStrategy::Poll { interval_secs: 30 },
        keep_alive: KeepAlive::Polls(4),
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 60,
        },
        fsevents_reinstate: false,
    }
}

fn status_meta() -> SourceMetadata {
    SourceMetadata {
        name: "status".into(),
        fields: vec![
            FieldSchema {
                name: "staged".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "unstaged".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "untracked".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "conflicted".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "dirty".into(),
                field_type: FieldType::Bool,
            },
        ],
        scope: SourceScope::PathScoped,
        invalidation: InvalidationStrategy::WatchAndPoll {
            patterns: vec![".git/index".into()],
            abs_paths: vec![],
            interval_secs: 60,
        },
        keep_alive: KeepAlive::Polls(2),
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 30,
        },
        fsevents_reinstate: true,
    }
}

// ── Source impls ──────────────────────────────────────────────────────────────

struct GitRefs {
    executor: Arc<dyn GitExecutor>,
}

impl GitRefs {
    fn new(executor: Arc<dyn GitExecutor>) -> Self {
        Self { executor }
    }
}

impl Source for GitRefs {
    fn metadata(&self) -> &SourceMetadata {
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(refs_meta)
    }

    fn execute(&self, path: Option<&str>) -> SourceResult {
        let Some(path) = path else {
            return SourceResult::new();
        };
        let dir = Path::new(path);

        let Some(status) = parse_git_status(dir, &*self.executor) else {
            return SourceResult::new();
        };
        let dirs = resolve_git_dir(dir);
        let stash_count = dirs.as_ref().map(count_stashes).unwrap_or(0);
        let (state, state_step, state_total) = dirs
            .as_ref()
            .map(detect_repo_state)
            .unwrap_or_else(|| ("clean".to_string(), 0, 0));
        let (commit, last_commit_ts, commit_summary) = get_head_info(dir, &*self.executor);
        let tag = get_nearest_tag(dir, &*self.executor);
        let (push_ahead, push_behind) = get_push_divergence(dir, &status.branch, &*self.executor);

        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let last_commit_age_secs = if last_commit_ts > 0 {
            now_secs.saturating_sub(last_commit_ts)
        } else {
            0
        };

        let mut result = SourceResult::new();
        result.insert("branch", Value::String(status.branch.clone()));
        result.insert("ahead", Value::Int(status.ahead));
        result.insert("behind", Value::Int(status.behind));
        result.insert("upstream", Value::String(status.upstream));
        result.insert("detached", Value::Bool(status.detached));
        result.insert("stash", Value::Int(stash_count));
        result.insert("state", Value::String(state));
        result.insert("state_step", Value::Int(state_step));
        result.insert("state_total", Value::Int(state_total));
        result.insert("commit", Value::String(commit));
        result.insert("tag", Value::String(tag));
        result.insert("last_commit_age_secs", Value::Int(last_commit_age_secs));
        result.insert("commit_summary", Value::String(commit_summary));
        result.insert("push_ahead", Value::Int(push_ahead));
        result.insert("push_behind", Value::Int(push_behind));
        result
    }

    fn canonical_path(&self, path: Option<&str>) -> Option<String> {
        walk_to_git(path)
    }
}

struct GitDiff {
    executor: Arc<dyn GitExecutor>,
}

impl GitDiff {
    fn new(executor: Arc<dyn GitExecutor>) -> Self {
        Self { executor }
    }
}

impl Source for GitDiff {
    fn metadata(&self) -> &SourceMetadata {
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(diff_meta)
    }

    fn execute(&self, path: Option<&str>) -> SourceResult {
        let Some(path) = path else {
            return SourceResult::new();
        };
        let dir = Path::new(path);

        let (lines_added, lines_removed) = diff_numstat(dir, &*self.executor);
        let (lines_staged_added, lines_staged_removed) = diff_numstat_staged(dir, &*self.executor);

        let mut result = SourceResult::new();
        result.insert("lines_added", Value::Int(lines_added));
        result.insert("lines_removed", Value::Int(lines_removed));
        result.insert("lines_staged_added", Value::Int(lines_staged_added));
        result.insert("lines_staged_removed", Value::Int(lines_staged_removed));
        result
    }

    fn canonical_path(&self, path: Option<&str>) -> Option<String> {
        walk_to_git(path)
    }
}

struct GitStatus {
    executor: Arc<dyn GitExecutor>,
}

impl GitStatus {
    fn new(executor: Arc<dyn GitExecutor>) -> Self {
        Self { executor }
    }
}

impl Source for GitStatus {
    fn metadata(&self) -> &SourceMetadata {
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(status_meta)
    }

    fn execute(&self, path: Option<&str>) -> SourceResult {
        let Some(path) = path else {
            return SourceResult::new();
        };
        let dir = Path::new(path);

        let Some(status) = parse_git_status(dir, &*self.executor) else {
            return SourceResult::new();
        };

        let dirty = status.staged > 0
            || status.unstaged > 0
            || status.untracked > 0
            || status.conflicted > 0;

        let mut result = SourceResult::new();
        result.insert("staged", Value::Int(status.staged));
        result.insert("unstaged", Value::Int(status.unstaged));
        result.insert("untracked", Value::Int(status.untracked));
        result.insert("conflicted", Value::Int(status.conflicted));
        result.insert("dirty", Value::Bool(dirty));
        result
    }

    fn canonical_path(&self, path: Option<&str>) -> Option<String> {
        walk_to_git(path)
    }
}

// ── Provider ──────────────────────────────────────────────────────────────────

pub struct GitProvider;

impl GitProvider {
    fn make_executor() -> Arc<dyn GitExecutor> {
        Arc::new(RealGitExecutor)
    }
}

impl Provider for GitProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "git".into(),
            sources: vec![refs_meta(), diff_meta(), status_meta()],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        let exec = Self::make_executor();
        vec![
            Box::new(GitRefs::new(Arc::clone(&exec))),
            Box::new(GitDiff::new(Arc::clone(&exec))),
            Box::new(GitStatus::new(exec)),
        ]
    }
}

/// Construct a `GitProvider` whose sources use the given executor.
/// Only available when the `test-helpers` feature is active (integration tests)
/// or in `cfg(test)` builds.
#[cfg(any(test, feature = "test-helpers"))]
pub fn git_provider_with_executor(executor: Arc<dyn GitExecutor>) -> impl Provider {
    struct GitProviderWithExecutor {
        executor: Arc<dyn GitExecutor>,
    }
    impl Provider for GitProviderWithExecutor {
        fn metadata(&self) -> ProviderMetadata {
            ProviderMetadata {
                name: "git".into(),
                sources: vec![refs_meta(), diff_meta(), status_meta()],
            }
        }
        fn sources(&self) -> Vec<Box<dyn Source>> {
            vec![
                Box::new(GitRefs::new(Arc::clone(&self.executor))),
                Box::new(GitDiff::new(Arc::clone(&self.executor))),
                Box::new(GitStatus::new(Arc::clone(&self.executor))),
            ]
        }
    }
    GitProviderWithExecutor { executor }
}

// ── Git internals ─────────────────────────────────────────────────────────────

struct ParsedGitStatus {
    branch: String,
    upstream: String,
    detached: bool,
    ahead: i64,
    behind: i64,
    staged: i64,
    unstaged: i64,
    untracked: i64,
    conflicted: i64,
}

fn parse_git_status(dir: &Path, executor: &dyn GitExecutor) -> Option<ParsedGitStatus> {
    let output = executor
        .run_git(
            dir,
            vec!["status".into(), "--porcelain=v2".into(), "--branch".into()],
        )
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut branch = String::new();
    let mut upstream = String::new();
    let mut detached = false;
    let mut ahead: i64 = 0;
    let mut behind: i64 = 0;
    let mut staged: i64 = 0;
    let mut unstaged: i64 = 0;
    let mut untracked: i64 = 0;
    let mut conflicted: i64 = 0;

    for line in stdout.lines() {
        if line.starts_with("# branch.head ") {
            let head = line.strip_prefix("# branch.head ").unwrap_or("");
            if head == "(detached)" {
                detached = true;
                branch = head.to_string();
            } else {
                branch = head.to_string();
            }
        } else if line.starts_with("# branch.upstream ") {
            upstream = line
                .strip_prefix("# branch.upstream ")
                .unwrap_or("")
                .to_string();
        } else if line.starts_with("# branch.ab ") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 {
                ahead = parts[2].trim_start_matches('+').parse().unwrap_or(0);
                behind = parts[3].trim_start_matches('-').parse().unwrap_or(0);
            }
        } else if line.starts_with("1 ") || line.starts_with("2 ") {
            let chars: Vec<char> = line.chars().collect();
            if chars.len() >= 4 {
                let x = chars[2];
                let y = chars[3];
                if x != '.' {
                    staged += 1;
                }
                if y != '.' {
                    unstaged += 1;
                }
            }
        } else if line.starts_with("u ") {
            conflicted += 1;
        } else if line.starts_with("? ") {
            untracked += 1;
        }
    }

    Some(ParsedGitStatus {
        branch,
        upstream,
        detached,
        ahead,
        behind,
        staged,
        unstaged,
        untracked,
        conflicted,
    })
}

fn count_stashes(dirs: &GitDirs) -> i64 {
    let stash_log = dirs.commondir.join("logs").join("refs").join("stash");
    std::fs::read_to_string(&stash_log)
        .map(|s| s.lines().count() as i64)
        .unwrap_or(0)
}

/// Returns (state_name, step, total).
fn detect_repo_state(dirs: &GitDirs) -> (String, i64, i64) {
    let git_dir = &dirs.gitdir;

    if git_dir.join("MERGE_HEAD").exists() {
        return ("merge".to_string(), 0, 0);
    }

    if git_dir.join("rebase-merge").exists() {
        let step = read_int_file(&git_dir.join("rebase-merge").join("msgnum"));
        let total = read_int_file(&git_dir.join("rebase-merge").join("end"));
        return ("rebase".to_string(), step, total);
    }

    if git_dir.join("rebase-apply").exists() {
        let step = read_int_file(&git_dir.join("rebase-apply").join("next"));
        let total = read_int_file(&git_dir.join("rebase-apply").join("last"));
        return ("rebase".to_string(), step, total);
    }

    if git_dir.join("CHERRY_PICK_HEAD").exists() {
        return ("cherry-pick".to_string(), 0, 0);
    }

    if git_dir.join("BISECT_LOG").exists() {
        return ("bisect".to_string(), 0, 0);
    }

    if git_dir.join("REVERT_HEAD").exists() {
        return ("revert".to_string(), 0, 0);
    }

    ("clean".to_string(), 0, 0)
}

fn read_int_file(path: &Path) -> i64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Runs `git diff --numstat` and returns (lines_added, lines_removed) summed across all files.
fn diff_numstat(dir: &Path, executor: &dyn GitExecutor) -> (i64, i64) {
    let output = executor.run_git(dir, vec!["diff".into(), "--numstat".into()]);
    parse_numstat_output(output)
}

/// Runs `git diff --cached --numstat` and returns (lines_added, lines_removed) summed.
fn diff_numstat_staged(dir: &Path, executor: &dyn GitExecutor) -> (i64, i64) {
    let output = executor.run_git(
        dir,
        vec!["diff".into(), "--cached".into(), "--numstat".into()],
    );
    parse_numstat_output(output)
}

fn parse_numstat_output(output: Result<std::process::Output, std::io::Error>) -> (i64, i64) {
    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return (0, 0),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut added: i64 = 0;
    let mut removed: i64 = 0;
    for line in stdout.lines() {
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() >= 2 {
            // Binary files show '-' instead of a number; skip those
            added += parts[0].parse::<i64>().unwrap_or(0);
            removed += parts[1].parse::<i64>().unwrap_or(0);
        }
    }
    (added, removed)
}

/// Runs `git log -1 --format="%h %ct %s"` and returns (short_hash, commit_timestamp, subject).
fn get_head_info(dir: &Path, executor: &dyn GitExecutor) -> (String, i64, String) {
    let output = executor.run_git(
        dir,
        vec!["log".into(), "-1".into(), "--format=%h %ct %s".into()],
    );

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return (String::new(), 0, String::new()),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.trim();
    let mut parts = line.splitn(3, ' ');
    let hash = parts.next().unwrap_or("").to_string();
    let ts: i64 = parts
        .next()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let subject = parts.next().unwrap_or("").to_string();
    (hash, ts, subject)
}

/// Runs `git describe --tags --abbrev=0` and returns the nearest tag or empty string.
fn get_nearest_tag(dir: &Path, executor: &dyn GitExecutor) -> String {
    let output = executor.run_git(
        dir,
        vec!["describe".into(), "--tags".into(), "--abbrev=0".into()],
    );

    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => String::new(),
    }
}

/// Returns (push_ahead, push_behind) relative to the push remote.
/// If no push remote is configured, returns (0, 0).
fn get_push_divergence(dir: &Path, branch: &str, executor: &dyn GitExecutor) -> (i64, i64) {
    if branch.is_empty() || branch == "(detached)" {
        return (0, 0);
    }

    // Resolve push remote: branch.<name>.pushRemote, then remote.pushDefault
    let push_remote = get_git_config(dir, &format!("branch.{branch}.pushRemote"), executor)
        .or_else(|| get_git_config(dir, "remote.pushDefault", executor));

    let push_remote = match push_remote {
        Some(r) => r,
        None => return (0, 0),
    };

    let refspec = format!("{push_remote}/{branch}");

    // Check the ref exists before rev-list
    let check = executor.run_git(
        dir,
        vec![
            "rev-parse".into(),
            "--verify".into(),
            "--quiet".into(),
            format!("refs/remotes/{refspec}"),
        ],
    );
    match check {
        Ok(o) if o.status.success() => {}
        _ => return (0, 0),
    }

    let output = executor.run_git(
        dir,
        vec![
            "rev-list".into(),
            "--count".into(),
            "--left-right".into(),
            format!("HEAD...{refspec}"),
        ],
    );

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return (0, 0),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = stdout.trim().split('\t').collect();
    if parts.len() == 2 {
        let ahead = parts[0].parse().unwrap_or(0);
        let behind = parts[1].parse().unwrap_or(0);
        (ahead, behind)
    } else {
        (0, 0)
    }
}

fn get_git_config(dir: &Path, key: &str, executor: &dyn GitExecutor) -> Option<String> {
    let output = executor
        .run_git(dir, vec!["config".into(), "--get".into(), key.into()])
        .ok()?;
    if output.status.success() {
        let val = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if val.is_empty() { None } else { Some(val) }
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn resolve_git_dir_plain_repo_uses_dot_git_for_both() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(".git")).unwrap();

        let dirs = resolve_git_dir(tmp.path()).expect("plain repo resolves");
        assert_eq!(dirs.gitdir, tmp.path().join(".git"));
        assert_eq!(dirs.commondir, tmp.path().join(".git"));
    }

    #[test]
    fn resolve_git_dir_missing_returns_none() {
        let tmp = TempDir::new().unwrap();
        assert!(resolve_git_dir(tmp.path()).is_none());
    }

    #[test]
    fn resolve_git_dir_linked_worktree_follows_pointer_and_commondir() {
        let tmp = TempDir::new().unwrap();
        let main_git = tmp.path().join("main").join(".git");
        let wt_gitdir = main_git.join("worktrees").join("wt");
        fs::create_dir_all(&wt_gitdir).unwrap();
        // commondir points back to <main>/.git, relative to the worktree gitdir.
        fs::write(wt_gitdir.join("commondir"), "../..\n").unwrap();

        let wt_root = tmp.path().join("wt");
        fs::create_dir_all(&wt_root).unwrap();
        fs::write(
            wt_root.join(".git"),
            format!("gitdir: {}\n", wt_gitdir.display()),
        )
        .unwrap();

        let dirs = resolve_git_dir(&wt_root).expect("worktree resolves");
        assert_eq!(dirs.gitdir, fs::canonicalize(&wt_gitdir).unwrap());
        assert_eq!(dirs.commondir, fs::canonicalize(&main_git).unwrap());
    }

    #[test]
    fn resolve_git_dir_relative_pointer_without_commondir_falls_back() {
        let tmp = TempDir::new().unwrap();
        // Submodule layout: superproject .git/modules/sub is the gitdir.
        let sub_gitdir = tmp.path().join(".git").join("modules").join("sub");
        fs::create_dir_all(&sub_gitdir).unwrap();
        let sub_root = tmp.path().join("sub");
        fs::create_dir_all(&sub_root).unwrap();
        fs::write(sub_root.join(".git"), "gitdir: ../.git/modules/sub\n").unwrap();

        let dirs = resolve_git_dir(&sub_root).expect("submodule resolves");
        assert_eq!(dirs.gitdir, fs::canonicalize(&sub_gitdir).unwrap());
        // No commondir file → commondir falls back to gitdir.
        assert_eq!(dirs.commondir, fs::canonicalize(&sub_gitdir).unwrap());
    }
}
