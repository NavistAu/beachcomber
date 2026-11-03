//! `git`: the plugin compiled into comb, for what a repository says through
//! git's own commands. `defaults/git.toml`'s Sources call it, one symbol each,
//! `plugin('git', '<symbol>', root)`; its `head` Source reads two files and
//! needs no plugin.
//!
//! | symbol | answers | commands |
//! |---|---|---|
//! | `refs` | `{upstream, ahead, behind, stash, state, state_step, state_total, commit, last_commit_age_secs, commit_summary, tag, push_ahead, push_behind}` | `status`, `log -1`, `describe`, `config` twice; with a push remote `rev-parse`, `rev-list` |
//! | `diff` | `{lines_added, lines_removed, lines_staged_added, lines_staged_removed}` | `diff --numstat`, `diff --cached --numstat` |
//! | `lineage` | `{branch_parent, lines_added_parent, lines_removed_parent, branch_default, lines_added_default, lines_removed_default}` | `rev-parse`s, `merge-base`, `diff --numstat` per comparison |
//! | `status` | `{staged, unstaged, untracked, conflicted, dirty}` | `status` |
//!
//! A symbol takes one argument, the work tree's root: its input is
//! `["<root>"]`. It answers an object and leaves out a fact it has no value
//! for, never `""`; `{}` for a directory that is not a work tree's root. `NULL`
//! is failure: `git status` failed where a symbol needs it, or it was given
//! anything else as arguments.
//!
//! A warm read cannot see a file read in here, so what must be seen by the next
//! read is `stat`ed by the Source's own lines (`defaults/git.toml`).
//!
//! Every command goes through [`RealGitExecutor`], the one place that says what
//! environment git runs in. An answer's lifetime and reentrancy are as
//! [`crate::plugin::sys`]'s.

use crate::boundaries::git::{GitExecutor, RealGitExecutor};
use crate::plugin::Facts;
use serde_json::{Value as Json, json};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

crate::plugin::symbols! {
    at_root;
    diff => diff_facts,
    lineage => lineage_facts,
    refs => refs_facts,
    status => status_facts,
}

/// `facts` of the root that `args` is, `[<root>]`, with the real git: `None`
/// for any other arguments.
fn at_root(facts: fn(&Path, &dyn GitExecutor) -> Option<Facts>, args: Vec<Json>) -> Option<Facts> {
    let [Json::String(root)] = args.as_slice() else {
        return None;
    };
    facts(Path::new(root), &RealGitExecutor)
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

/// What `refs` answers: `None` when `git status` fails. What the repo does not
/// have is absent, never "" or 0: an upstream (and with it ahead/behind), a
/// commit, a tag, a push remote, a stepped operation in progress. Counts
/// (stashes) are values.
fn refs_facts(dir: &Path, executor: &dyn GitExecutor) -> Option<Facts> {
    // Outside a work tree there is nothing to report. Resolving the dirs is
    // also the guard: this source needs them anyway, so it costs one stat
    // rather than one plus a `git status` that could only fail.
    let Some(dirs) = resolve_git_dir(dir) else {
        return Some(Facts::new());
    };

    let status = parse_git_status(dir, executor)?;
    let stash_count = count_stashes(&dirs);
    let (state, steps) = detect_repo_state(&dirs);
    let head = get_head_info(dir, executor);
    let tag = get_nearest_tag(dir, executor);
    let push = get_push_divergence(dir, &status.branch, executor);

    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let mut facts = Facts::new();
    if let Some(upstream) = status.upstream {
        facts.insert("upstream".into(), json!(upstream));
        if let Some((ahead, behind)) = status.ab {
            facts.insert("ahead".into(), json!(ahead));
            facts.insert("behind".into(), json!(behind));
        }
    }
    facts.insert("stash".into(), json!(stash_count));
    facts.insert("state".into(), json!(state));
    if let Some((step, total)) = steps {
        facts.insert("state_step".into(), json!(step));
        facts.insert("state_total".into(), json!(total));
    }
    if let Some(head) = head {
        facts.insert("commit".into(), json!(head.hash));
        facts.insert(
            "last_commit_age_secs".into(),
            json!(now_secs.saturating_sub(head.timestamp)),
        );
        facts.insert("commit_summary".into(), json!(head.subject));
    }
    if let Some(tag) = tag {
        facts.insert("tag".into(), json!(tag));
    }
    if let Some((push_ahead, push_behind)) = push {
        facts.insert("push_ahead".into(), json!(push_ahead));
        facts.insert("push_behind".into(), json!(push_behind));
    }
    Some(facts)
}

/// What `diff` answers.
fn diff_facts(dir: &Path, executor: &dyn GitExecutor) -> Option<Facts> {
    // `git diff --numstat` fails outside a work tree and the parse turns a
    // failure into `(0, 0)`, so without this guard every non-repository
    // directory answers with four zeros: a hit, not the miss it is.
    if !is_work_tree_root(dir) {
        return Some(Facts::new());
    }

    let (lines_added, lines_removed) = diff_numstat(dir, executor);
    let (lines_staged_added, lines_staged_removed) = diff_numstat_staged(dir, executor);

    let mut facts = Facts::new();
    facts.insert("lines_added".into(), json!(lines_added));
    facts.insert("lines_removed".into(), json!(lines_removed));
    facts.insert("lines_staged_added".into(), json!(lines_staged_added));
    facts.insert("lines_staged_removed".into(), json!(lines_staged_removed));
    Some(facts)
}

/// What `lineage` answers: branch-comparison line counts, how much the
/// checked-out branch has diverged from the branch it was forked off
/// (`branch_parent`) and from the repository's default branch
/// (`branch_default`).
///
/// Both counts are taken from the commit DAG, `git diff --numstat <point>..HEAD`
/// where `<point>` is a merge base, so they agree with what `git log --graph`
/// shows. The reflog only refines *which* branch the parent is.
fn lineage_facts(dir: &Path, executor: &dyn GitExecutor) -> Option<Facts> {
    let Some(dirs) = resolve_git_dir(dir) else {
        return Some(Facts::new());
    };

    // Detached HEAD has no branch, so it has neither a parent nor a
    // meaningful position relative to one. Emit nothing.
    let (Some(branch), false) = parse_head(&dirs) else {
        return Some(Facts::new());
    };

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

    let mut facts = Facts::new();

    if let (Some((name, _)), Some((added, removed))) = (&parent, parent_counts) {
        facts.insert("branch_parent".into(), json!(name));
        facts.insert("lines_added_parent".into(), json!(added));
        facts.insert("lines_removed_parent".into(), json!(removed));
    }

    if let Some((name, rev)) = &base {
        // Whenever the parent fell back to the default branch the two revs
        // are identical: reuse the counts rather than spawning the same
        // subprocesses twice.
        let same_as_parent = parent.as_ref().is_some_and(|(_, p)| p == rev);
        let (added, removed) = match (same_as_parent, parent_counts) {
            (true, Some(counts)) => counts,
            _ => lines_since(dir, rev, executor),
        };
        facts.insert("branch_default".into(), json!(name));
        facts.insert("lines_added_default".into(), json!(added));
        facts.insert("lines_removed_default".into(), json!(removed));
    }

    Some(facts)
}

/// What `status` answers: `None` when `git status` fails.
fn status_facts(dir: &Path, executor: &dyn GitExecutor) -> Option<Facts> {
    // Outside a work tree there is nothing to report: the filesystem probe
    // says so without spawning anything.
    if !is_work_tree_root(dir) {
        return Some(Facts::new());
    }

    let status = parse_git_status(dir, executor)?;

    let dirty =
        status.staged > 0 || status.unstaged > 0 || status.untracked > 0 || status.conflicted > 0;

    let mut facts = Facts::new();
    facts.insert("staged".into(), json!(status.staged));
    facts.insert("unstaged".into(), json!(status.unstaged));
    facts.insert("untracked".into(), json!(status.untracked));
    facts.insert("conflicted".into(), json!(status.conflicted));
    facts.insert("dirty".into(), json!(dirty));
    Some(facts)
}

// ── Git internals ─────────────────────────────────────────────────────────────

/// Parse the HEAD file of the given gitdir.
///
/// Returns (branch, detached):
/// - `ref: refs/heads/<name>` → branch=<name>, detached=false
/// - 40/64-hex line (detached HEAD) → no branch, detached=true
/// - missing/other → no branch, detached=false
fn parse_head(dirs: &GitDirs) -> (Option<String>, bool) {
    let head_path = dirs.gitdir.join("HEAD");
    let Ok(contents) = std::fs::read_to_string(&head_path) else {
        return (None, false);
    };
    let line = contents.trim();
    if let Some(branch) = line.strip_prefix("ref: refs/heads/") {
        return (Some(branch.to_string()), false);
    }
    // Detached HEAD: 40-char (SHA-1) or 64-char (SHA-256) hex string
    if (line.len() == 40 || line.len() == 64) && line.chars().all(|c| c.is_ascii_hexdigit()) {
        return (None, true);
    }
    (None, false)
}

struct ParsedGitStatus {
    branch: String,
    /// The branch's upstream, when it has one.
    upstream: Option<String>,
    /// (ahead, behind) of the upstream, reported only when there is one.
    ab: Option<(i64, i64)>,
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
    let mut upstream = None;
    let mut ab = None;
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
        } else if let Some(name) = line.strip_prefix("# branch.upstream ") {
            upstream = Some(name.to_string());
        } else if line.starts_with("# branch.ab ") {
            // Present only when the branch has an upstream to compare with.
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4
                && let (Ok(ahead), Ok(behind)) = (
                    parts[2].trim_start_matches('+').parse(),
                    parts[3].trim_start_matches('-').parse(),
                )
            {
                ab = Some((ahead, behind));
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
        ab,
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

/// Returns (state_name, (step, total) for a stepped operation in progress).
fn detect_repo_state(dirs: &GitDirs) -> (String, Option<(i64, i64)>) {
    let git_dir = &dirs.gitdir;

    if git_dir.join("MERGE_HEAD").exists() {
        return ("merge".to_string(), None);
    }

    if git_dir.join("rebase-merge").exists() {
        let steps = read_steps(
            &git_dir.join("rebase-merge").join("msgnum"),
            &git_dir.join("rebase-merge").join("end"),
        );
        return ("rebase".to_string(), steps);
    }

    if git_dir.join("rebase-apply").exists() {
        let steps = read_steps(
            &git_dir.join("rebase-apply").join("next"),
            &git_dir.join("rebase-apply").join("last"),
        );
        return ("rebase".to_string(), steps);
    }

    if git_dir.join("CHERRY_PICK_HEAD").exists() {
        return ("cherry-pick".to_string(), None);
    }

    if git_dir.join("BISECT_LOG").exists() {
        return ("bisect".to_string(), None);
    }

    if git_dir.join("REVERT_HEAD").exists() {
        return ("revert".to_string(), None);
    }

    ("clean".to_string(), None)
}

/// A stepped operation's (step, total), when both files read as numbers.
fn read_steps(step: &Path, total: &Path) -> Option<(i64, i64)> {
    Some((read_int_file(step)?, read_int_file(total)?))
}

fn read_int_file(path: &Path) -> Option<i64> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
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

/// The last commit: its short hash, timestamp and subject.
struct HeadCommit {
    hash: String,
    timestamp: i64,
    subject: String,
}

/// Runs `git log -1 --format="%h %ct %s"`: the last commit, or `None` in a
/// repo with no commits yet.
fn get_head_info(dir: &Path, executor: &dyn GitExecutor) -> Option<HeadCommit> {
    let output = executor.run_git(
        dir,
        vec!["log".into(), "-1".into(), "--format=%h %ct %s".into()],
    );

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return None,
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.trim();
    let mut parts = line.splitn(3, ' ');
    let hash = parts.next().filter(|h| !h.is_empty())?.to_string();
    let timestamp: i64 = parts.next().and_then(|s| s.trim().parse().ok())?;
    let subject = parts.next().unwrap_or("").to_string();
    Some(HeadCommit {
        hash,
        timestamp,
        subject,
    })
}

/// Runs `git describe --tags --abbrev=0`: the nearest tag, or `None` when
/// there is none.
fn get_nearest_tag(dir: &Path, executor: &dyn GitExecutor) -> Option<String> {
    let output = executor.run_git(
        dir,
        vec!["describe".into(), "--tags".into(), "--abbrev=0".into()],
    );

    match output {
        Ok(o) if o.status.success() => {
            Some(String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|t| !t.is_empty())
        }
        _ => None,
    }
}

/// Returns (push_ahead, push_behind) relative to the push remote, or `None`
/// when there is no push remote, or no branch on it, to compare with.
fn get_push_divergence(dir: &Path, branch: &str, executor: &dyn GitExecutor) -> Option<(i64, i64)> {
    if branch.is_empty() || branch == "(detached)" {
        return None;
    }

    // Resolve push remote: branch.<name>.pushRemote, then remote.pushDefault
    let push_remote = get_git_config(dir, &format!("branch.{branch}.pushRemote"), executor)
        .or_else(|| get_git_config(dir, "remote.pushDefault", executor));

    let push_remote = push_remote?;

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
        _ => return None,
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
        _ => return None,
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = stdout.trim().split('\t').collect();
    match parts.as_slice() {
        [ahead, behind] => Some((ahead.parse().ok()?, behind.parse().ok()?)),
        _ => None,
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

    #[test]
    fn the_table_is_sorted_and_names_each_symbol_once() {
        let names: Vec<&str> = SYMBOLS.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["diff", "lineage", "refs", "status"]);
    }

    /// Git finds the repository from its directory, whatever the daemon's own
    /// environment says: an inherited `GIT_DIR` would answer for another repo.
    #[test]
    fn an_inherited_git_dir_is_not_used() {
        let tmp = TempDir::new().unwrap();
        RealGitExecutor
            .run_git(tmp.path(), vec!["init".into(), "-q".into()])
            .unwrap();
        temp_env::with_vars(
            [
                ("GIT_DIR", Some("/nonexistent")),
                ("GIT_WORK_TREE", Some("/nonexistent")),
            ],
            || {
                let facts = status_facts(tmp.path(), &RealGitExecutor).expect("git status ran");
                assert_eq!(facts["dirty"], json!(false));
            },
        );
    }

    /// One argument, the root: anything else is the failure `NULL` is.
    #[test]
    fn a_symbol_takes_one_string() {
        for args in ["[]", "[1]", "[\"/a\",\"/b\"]", "not json"] {
            let input = std::ffi::CString::new(args).unwrap();
            for (name, f) in SYMBOLS {
                // SAFETY: the contract's input, a NUL-terminated string.
                let out = unsafe { f(input.as_ptr()) };
                assert!(out.is_null(), "{name} of {args}");
            }
        }
    }

    /// A directory that is not a work tree's root answers `{}` through the
    /// real symbol, spawning nothing.
    #[test]
    fn outside_a_repository_every_symbol_answers_nothing() {
        let tmp = TempDir::new().unwrap();
        let input =
            std::ffi::CString::new(json!([tmp.path().to_str().unwrap()]).to_string()).unwrap();
        for (name, f) in SYMBOLS {
            // SAFETY: the contract's input, a NUL-terminated string.
            let out = unsafe { f(input.as_ptr()) };
            assert!(!out.is_null(), "{name}");
            // SAFETY: a non-NULL answer is a NUL-terminated string.
            let text = unsafe { std::ffi::CStr::from_ptr(out) }.to_str().unwrap();
            assert_eq!(text, "{}", "{name}");
        }
    }
}

#[cfg(test)]
#[path = "seam_tests.rs"]
mod seam_tests;
