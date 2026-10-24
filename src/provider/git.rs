use crate::boundaries::git::{GitExecutor, RealGitExecutor};
use crate::provider::{
    FailbackConfig, FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    Source, SourceMetadata, SourceResult, Value,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Walk upwards from `dir` looking for a directory that contains `.git`: the
/// instance's root. `None` when no repo root is found before reaching the
/// filesystem root, which is the instance at `~`.
///
/// Uses file-system traversal rather than shelling out to `git rev-parse` so
/// finding the root stays cheap for the common case (every demand path).
fn walk_to_git(dir: &str) -> Result<Option<String>, String> {
    Ok(find_repo_root(Path::new(dir)))
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

/// Whether `dir` is the root of a git work tree.
///
/// Every git Source must answer nothing outside one. `GitDiff` was the one that
/// did not: `git diff --numstat` fails outside a work tree and the parse turns
/// that into `(0, 0)`, so it reported four zeroes for any directory at all —
/// a miss dressed up as a hit, and the pathless global entry (whose directory
/// is `$HOME`) is read by every consumer that names no directory.
///
/// A filesystem probe, not a `git rev-parse`, so the guard costs no process —
/// and for `GitStatus` it saves the `git status` it would otherwise spawn only
/// to watch fail. `GitHead`, `GitRefs` and `GitLineage` hold the same shape
/// inline, since they need the `GitDirs` themselves. The root is the only
/// directory a rooted instance is handed: `root` walks to it before the daemon
/// keys anything. The instance at `~` is handed `$HOME`, which may not be one.
fn is_work_tree_root(dir: &Path) -> bool {
    resolve_git_dir(dir).is_some()
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

fn head_meta() -> SourceMetadata {
    SourceMetadata {
        name: "head".into(),
        fields: vec![
            FieldSchema {
                name: "branch".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "detached".into(),
                field_type: FieldType::Bool,
            },
        ],
        invalidation: InvalidationStrategy::Watch {
            patterns: vec![".git".into()],
            abs_paths: vec![],
        },
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 60,
        },
        fsevents_reinstate: true,
        decay: None,
        budget_execute: crate::scheduler::lifecycle::DEFAULT_BUDGET_EXECUTE,
    }
}

fn refs_meta() -> SourceMetadata {
    SourceMetadata {
        name: "refs".into(),
        fields: vec![
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
        invalidation: InvalidationStrategy::Watch {
            patterns: vec![".git".into()],
            abs_paths: vec![],
        },
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 60,
        },
        fsevents_reinstate: true,
        decay: None,
        budget_execute: crate::scheduler::lifecycle::DEFAULT_BUDGET_EXECUTE,
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
        invalidation: InvalidationStrategy::Poll {
            interval_secs: 30,
            count: 4,
        },
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 60,
        },
        fsevents_reinstate: false,
        decay: None,
        budget_execute: crate::scheduler::lifecycle::DEFAULT_BUDGET_EXECUTE,
    }
}

fn lineage_meta() -> SourceMetadata {
    SourceMetadata {
        name: "lineage".into(),
        fields: vec![
            FieldSchema {
                name: "branch_parent".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "lines_added_parent".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "lines_removed_parent".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "branch_default".into(),
                field_type: FieldType::String,
            },
            FieldSchema {
                name: "lines_added_default".into(),
                field_type: FieldType::Int,
            },
            FieldSchema {
                name: "lines_removed_default".into(),
                field_type: FieldType::Int,
            },
        ],
        // Poll, like `diff`: the computation is a handful of subprocess spawns,
        // far too costly to run on every `.git` event. The values only move when
        // refs move, so 60s (twice `diff`'s interval) is ample; `count: 2` gives a
        // 120s Active window.
        invalidation: InvalidationStrategy::Poll {
            interval_secs: 60,
            count: 2,
        },
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 60,
        },
        fsevents_reinstate: false,
        decay: None,
        budget_execute: crate::scheduler::lifecycle::DEFAULT_BUDGET_EXECUTE,
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
        invalidation: InvalidationStrategy::WatchAndPoll {
            patterns: vec![".git/index".into()],
            abs_paths: vec![],
            interval_secs: 60,
            count: 2,
        },
        failback: FailbackConfig {
            reattempts: 3,
            interval_secs: 30,
        },
        fsevents_reinstate: true,
        decay: None,
        budget_execute: crate::scheduler::lifecycle::DEFAULT_BUDGET_EXECUTE,
    }
}

// ── Source impls ──────────────────────────────────────────────────────────────

/// Read-always source: parses `<gitdir>/HEAD` directly, no subprocess.
/// Returns `branch` (String) and `detached` (Bool) on every request.
struct GitHead;

impl GitHead {
    /// Parse the HEAD file of the given gitdir.
    ///
    /// Returns (branch, detached):
    /// - `ref: refs/heads/<name>` → branch=<name>, detached=false
    /// - 40/64-hex line (detached HEAD) → branch="", detached=true
    /// - missing/other → branch="", detached=false
    fn parse_head(dirs: &GitDirs) -> (String, bool) {
        let head_path = dirs.gitdir.join("HEAD");
        let Ok(contents) = std::fs::read_to_string(&head_path) else {
            return (String::new(), false);
        };
        let line = contents.trim();
        if let Some(branch) = line.strip_prefix("ref: refs/heads/") {
            return (branch.to_string(), false);
        }
        // Detached HEAD: 40-char (SHA-1) or 64-char (SHA-256) hex string
        if (line.len() == 40 || line.len() == 64) && line.chars().all(|c| c.is_ascii_hexdigit()) {
            return (String::new(), true);
        }
        (String::new(), false)
    }
}

impl Source for GitHead {
    fn metadata(&self) -> &SourceMetadata {
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(head_meta)
    }

    fn execute(&self, path: Option<&str>) -> Result<SourceResult, String> {
        let Some(path) = path else {
            return Err("git: no directory to run in".into());
        };
        // Only emit fields when we can confirm a git repo exists: outside one
        // the result is empty, which stores no fields and so reads as a miss.
        let Some(dirs) = resolve_git_dir(Path::new(path)) else {
            return Ok(SourceResult::new());
        };
        let (branch, detached) = Self::parse_head(&dirs);
        let mut result = SourceResult::new();
        result.insert("branch", Value::String(branch));
        result.insert("detached", Value::Bool(detached));
        Ok(result)
    }

    fn root(&self, dir: &str) -> Result<Option<String>, String> {
        walk_to_git(dir)
    }

    fn root_key(&self) -> String {
        "git".into()
    }

    fn has_root(&self) -> bool {
        true
    }

    fn read_always(&self) -> bool {
        true
    }
}

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

    fn execute(&self, path: Option<&str>) -> Result<SourceResult, String> {
        let Some(path) = path else {
            return Err("git: no directory to run in".into());
        };
        let dir = Path::new(path);
        // Outside a work tree there is nothing to report. Resolving the dirs is
        // also the guard — this source needs them anyway, so it costs one stat
        // rather than one plus a `git status` that could only fail.
        let Some(dirs) = resolve_git_dir(dir) else {
            return Ok(SourceResult::new());
        };

        let Some(status) = parse_git_status(dir, &*self.executor) else {
            return Err("git status failed".into());
        };
        let stash_count = count_stashes(&dirs);
        let (state, state_step, state_total) = detect_repo_state(&dirs);
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
        result.insert("ahead", Value::Int(status.ahead));
        result.insert("behind", Value::Int(status.behind));
        result.insert("upstream", Value::String(status.upstream));
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
        Ok(result)
    }

    fn root(&self, dir: &str) -> Result<Option<String>, String> {
        walk_to_git(dir)
    }

    fn root_key(&self) -> String {
        "git".into()
    }

    fn has_root(&self) -> bool {
        true
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

    fn execute(&self, path: Option<&str>) -> Result<SourceResult, String> {
        let Some(path) = path else {
            return Err("git: no directory to run in".into());
        };
        let dir = Path::new(path);
        // `git diff --numstat` fails outside a work tree and the parse turns a
        // failure into `(0, 0)`, so without this guard every non-repository
        // directory answers with four zeros — a hit, not the miss it is.
        if !is_work_tree_root(dir) {
            return Ok(SourceResult::new());
        }

        let (lines_added, lines_removed) = diff_numstat(dir, &*self.executor);
        let (lines_staged_added, lines_staged_removed) = diff_numstat_staged(dir, &*self.executor);

        let mut result = SourceResult::new();
        result.insert("lines_added", Value::Int(lines_added));
        result.insert("lines_removed", Value::Int(lines_removed));
        result.insert("lines_staged_added", Value::Int(lines_staged_added));
        result.insert("lines_staged_removed", Value::Int(lines_staged_removed));
        Ok(result)
    }

    fn root(&self, dir: &str) -> Result<Option<String>, String> {
        walk_to_git(dir)
    }

    fn root_key(&self) -> String {
        "git".into()
    }

    fn has_root(&self) -> bool {
        true
    }
}

/// Branch-comparison line counts: how much the checked-out branch has diverged
/// from the branch it was forked off (`branch_parent`) and from the repository's
/// default branch (`branch_default`).
///
/// Both counts are taken from the commit DAG — `git diff --numstat <point>..HEAD`
/// where `<point>` is a merge base — so they agree with what `git log --graph`
/// shows. The reflog only refines *which* branch the parent is.
struct GitLineage {
    executor: Arc<dyn GitExecutor>,
}

impl GitLineage {
    fn new(executor: Arc<dyn GitExecutor>) -> Self {
        Self { executor }
    }
}

impl Source for GitLineage {
    fn metadata(&self) -> &SourceMetadata {
        static M: OnceLock<SourceMetadata> = OnceLock::new();
        M.get_or_init(lineage_meta)
    }

    fn execute(&self, path: Option<&str>) -> Result<SourceResult, String> {
        let Some(path) = path else {
            return Err("git: no directory to run in".into());
        };
        let dir = Path::new(path);
        let Some(dirs) = resolve_git_dir(dir) else {
            return Ok(SourceResult::new());
        };

        // Detached HEAD has no branch, so it has neither a parent nor a
        // meaningful position relative to one. Emit nothing.
        let (branch, detached) = GitHead::parse_head(&dirs);
        if detached || branch.is_empty() {
            return Ok(SourceResult::new());
        }

        let executor = &*self.executor;
        let base = detect_default_branch(&dirs, dir, executor);

        // Parent: the reflog's recorded fork source when it names a local branch
        // that still exists and is not this branch; otherwise the default branch.
        let parent = read_reflog_parent(&dirs, &branch)
            .filter(|p| p != &branch && local_branch_exists(dir, p, executor))
            .map(|p| (p.clone(), p))
            .or_else(|| base.clone());

        let parent_counts = parent
            .as_ref()
            .map(|(_, rev)| lines_since(dir, rev, executor));

        let mut result = SourceResult::new();

        if let (Some((name, _)), Some((added, removed))) = (&parent, parent_counts) {
            result.insert("branch_parent", Value::String(name.clone()));
            result.insert("lines_added_parent", Value::Int(added));
            result.insert("lines_removed_parent", Value::Int(removed));
        }

        if let Some((name, rev)) = &base {
            // Whenever the parent fell back to the default branch the two revs
            // are identical — reuse the counts rather than spawning the same
            // subprocesses twice.
            let same_as_parent = parent.as_ref().is_some_and(|(_, p)| p == rev);
            let (added, removed) = match (same_as_parent, parent_counts) {
                (true, Some(counts)) => counts,
                _ => lines_since(dir, rev, executor),
            };
            result.insert("branch_default", Value::String(name.clone()));
            result.insert("lines_added_default", Value::Int(added));
            result.insert("lines_removed_default", Value::Int(removed));
        }

        Ok(result)
    }

    fn root(&self, dir: &str) -> Result<Option<String>, String> {
        walk_to_git(dir)
    }

    fn root_key(&self) -> String {
        "git".into()
    }

    fn has_root(&self) -> bool {
        true
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

    fn execute(&self, path: Option<&str>) -> Result<SourceResult, String> {
        let Some(path) = path else {
            return Err("git: no directory to run in".into());
        };
        let dir = Path::new(path);
        // Outside a work tree there is nothing to report: `git status` fails
        // and the parse already turns that into a miss, but the filesystem
        // probe says so without spawning anything.
        if !is_work_tree_root(dir) {
            return Ok(SourceResult::new());
        }

        let Some(status) = parse_git_status(dir, &*self.executor) else {
            return Err("git status failed".into());
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
        Ok(result)
    }

    fn root(&self, dir: &str) -> Result<Option<String>, String> {
        walk_to_git(dir)
    }

    fn root_key(&self) -> String {
        "git".into()
    }

    fn has_root(&self) -> bool {
        true
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
            sources: vec![
                head_meta(),
                refs_meta(),
                diff_meta(),
                lineage_meta(),
                status_meta(),
            ],
        }
    }

    fn sources(&self) -> Vec<Box<dyn Source>> {
        let exec = Self::make_executor();
        vec![
            Box::new(GitHead),
            Box::new(GitRefs::new(Arc::clone(&exec))),
            Box::new(GitDiff::new(Arc::clone(&exec))),
            Box::new(GitLineage::new(Arc::clone(&exec))),
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
                sources: vec![
                    head_meta(),
                    refs_meta(),
                    diff_meta(),
                    lineage_meta(),
                    status_meta(),
                ],
            }
        }
        fn sources(&self) -> Vec<Box<dyn Source>> {
            vec![
                Box::new(GitHead),
                Box::new(GitRefs::new(Arc::clone(&self.executor))),
                Box::new(GitDiff::new(Arc::clone(&self.executor))),
                Box::new(GitLineage::new(Arc::clone(&self.executor))),
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
    let mut ahead: i64 = 0;
    let mut behind: i64 = 0;
    let mut staged: i64 = 0;
    let mut unstaged: i64 = 0;
    let mut untracked: i64 = 0;
    let mut conflicted: i64 = 0;

    for line in stdout.lines() {
        if line.starts_with("# branch.head ") {
            branch = line
                .strip_prefix("# branch.head ")
                .unwrap_or("")
                .to_string();
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

/// Runs `git diff --numstat <range>` and returns (lines_added, lines_removed) summed.
fn diff_numstat_range(dir: &Path, range: &str, executor: &dyn GitExecutor) -> (i64, i64) {
    let output = executor.run_git(
        dir,
        vec!["diff".into(), "--numstat".into(), range.to_string()],
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

// ── Lineage internals ─────────────────────────────────────────────────────────

/// The branch a branch was forked from, per the oldest entry of its reflog.
///
/// Branch reflogs live in the **common** dir (`logs/refs/heads/<branch>`) — only
/// `logs/HEAD` is per-worktree — so a linked worktree reads the same file as the
/// main checkout. The oldest entry's message is `branch: Created from <X>`,
/// where `<X>` is verbatim what the user passed as the start point. `git
/// checkout -b <new>` with no start point records `HEAD`, and a raw revision
/// records a hash; neither names a branch, so both yield `None` and the caller
/// falls back to the default branch.
fn read_reflog_parent(dirs: &GitDirs, branch: &str) -> Option<String> {
    // `branch` comes from HEAD verbatim; keep a malformed one from walking out
    // of the reflog directory. Git's own refname rules already forbid these.
    if branch
        .split('/')
        .any(|c| c.is_empty() || c == "." || c == "..")
    {
        return None;
    }
    let mut reflog = dirs.commondir.join("logs").join("refs").join("heads");
    for component in branch.split('/') {
        reflog.push(component);
    }
    let contents = std::fs::read_to_string(&reflog).ok()?;

    // `<old> <new> <name> <email> <ts> <tz>\t<message>`
    let message = contents.lines().next()?.split_once('\t')?.1;
    let candidate = message.strip_prefix("branch: Created from ")?.trim();

    if candidate.is_empty() || candidate == "HEAD" || is_hex_oid(candidate) {
        return None;
    }
    Some(candidate.to_string())
}

fn is_hex_oid(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// The repository's default branch as (display name, revision to compare against).
///
/// Prefers `<commondir>/refs/remotes/origin/HEAD` — a loose symref file reading
/// `ref: refs/remotes/origin/<name>` — in which case the comparison revision is
/// the remote-tracking `origin/<name>`, so a checkout sitting *on* the default
/// branch still measures the work it has not pushed. Without it, falls back to
/// whichever of `main` / `master` resolves locally.
fn detect_default_branch(
    dirs: &GitDirs,
    dir: &Path,
    executor: &dyn GitExecutor,
) -> Option<(String, String)> {
    let origin_head = dirs
        .commondir
        .join("refs")
        .join("remotes")
        .join("origin")
        .join("HEAD");
    if let Ok(contents) = std::fs::read_to_string(&origin_head)
        && let Some(name) = contents
            .trim()
            .strip_prefix("ref: refs/remotes/origin/")
            .filter(|n| !n.is_empty())
    {
        return Some((name.to_string(), format!("origin/{name}")));
    }

    ["main", "master"]
        .into_iter()
        .find(|name| local_branch_exists(dir, name, executor))
        .map(|name| (name.to_string(), name.to_string()))
}

/// Whether `refs/heads/<branch>` resolves. Asks git rather than stat-ing
/// `refs/heads/` so packed refs — the normal state of a fresh clone — count.
fn local_branch_exists(dir: &Path, branch: &str, executor: &dyn GitExecutor) -> bool {
    let output = executor.run_git(
        dir,
        vec![
            "rev-parse".into(),
            "--verify".into(),
            "--quiet".into(),
            format!("refs/heads/{branch}"),
        ],
    );
    matches!(output, Ok(o) if o.status.success())
}

/// Lines added/removed on HEAD since it diverged from `rev`.
///
/// The divergence point is `git merge-base --fork-point <rev> HEAD`, which uses
/// `<rev>`'s reflog to pick the commit HEAD actually branched from even after
/// `<rev>` has been rebased. A pruned reflog or a fresh clone leaves it with
/// nothing to say (empty output), so plain `git merge-base` takes over.
fn lines_since(dir: &Path, rev: &str, executor: &dyn GitExecutor) -> (i64, i64) {
    let Some(point) = merge_base(dir, rev, executor) else {
        return (0, 0);
    };
    diff_numstat_range(dir, &format!("{point}..HEAD"), executor)
}

fn merge_base(dir: &Path, rev: &str, executor: &dyn GitExecutor) -> Option<String> {
    let fork_point = executor.run_git(
        dir,
        vec![
            "merge-base".into(),
            "--fork-point".into(),
            rev.to_string(),
            "HEAD".into(),
        ],
    );
    if let Some(point) = first_line_if_success(fork_point) {
        return Some(point);
    }

    let plain = executor.run_git(
        dir,
        vec!["merge-base".into(), rev.to_string(), "HEAD".into()],
    );
    first_line_if_success(plain)
}

fn first_line_if_success(output: Result<std::process::Output, std::io::Error>) -> Option<String> {
    let output = output.ok().filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next()?.trim();
    if line.is_empty() {
        None
    } else {
        Some(line.to_string())
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
