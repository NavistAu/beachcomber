//! `git`: the plugin compiled into comb, for what a repository says through
//! its files and git's own commands. `defaults/git.toml`'s Sources call it, one
//! symbol each, `plugin('git', '<symbol>', root)`.
//!
//! | symbol | answers | commands | a change to these runs it again |
//! |---|---|---|---|
//! | `head` | `{branch, detached}` | none | `HEAD` |
//! | `refs` | `{stash, state, state_step, state_total, commit, last_commit_age_secs, commit_summary, tag, push_ahead, push_behind}` | `log -1`, `describe`, `config`; with a push remote `rev-parse`, `rev-list` | what `HEAD` resolves through, the tags, the stash's log, a stepped operation's files, `config`, the push remote's ref |
//! | `diff` | `{lines_added, lines_removed, lines_staged_added, lines_staged_removed}` | `diff --numstat`, `diff --cached --numstat` | what `HEAD` resolves through, the index |
//! | `lineage` | `{branch_parent, lines_added_parent, lines_removed_parent, branch_default, lines_added_default, lines_removed_default}` | `merge-base`, `diff --numstat` per comparison; a `rev-parse` for a branch that has no loose ref | what `HEAD` resolves through, the branch's reflog, the parent's and the default branch's refs |
//! | `status` | `{upstream, ahead, behind, staged, unstaged, untracked, conflicted, dirty}` | `status` | what `HEAD` resolves through, the index, `config`, the upstream's ref |
//!
//! A symbol takes one argument, the work tree's root: its input is
//! `["<root>"]`. It answers an object and leaves out a fact it has no value
//! for, never `""`; `{}` for a directory that is not a work tree's root. `NULL`
//! is failure: `git status` failed where a symbol needs it, or it was given
//! anything else as arguments.
//!
//! A symbol notes each file it reads, stamped before it reads it, in the
//! account of the run it is called in ([`crate::run::reads_here`]), as a
//! Source's own lines do: a read checks exactly those files, and a Source runs
//! again only when what it says can have changed. A lockfile coming and going
//! is nothing here. What git's own processes read beyond these (an edit in the
//! work tree, the global config) is the poll's.
//!
//! Every command goes through [`RealGitExecutor`], the one place that says what
//! environment git runs in. An answer's lifetime and reentrancy are as
//! [`crate::plugin::sys`]'s.

use crate::boundaries::git::{GitExecutor, RealGitExecutor};
use crate::plugin::Facts;
use libbeachcomber::fs::Touch;
use serde_json::{Value as Json, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

crate::plugin::symbols! {
    at_root;
    diff => diff_facts,
    head => head_facts,
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

/// `path` is about to be read: noted for the run this is called in, if any.
fn note(path: &Path) {
    if let Some(reads) = crate::run::reads_here() {
        reads.touch(path);
    }
}

/// Stamps already taken: noted for the run this is called in, if any.
fn note_taken(touches: impl IntoIterator<Item = Touch>) {
    if let Some(reads) = crate::run::reads_here() {
        reads.keep(touches);
    }
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
    // Whether it is there, and which kind: a lockfile made in it is not a change.
    note_taken([Touch::presence(&dot_git)]);
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
    note(&dot_git);
    let contents = std::fs::read_to_string(&dot_git).ok()?;
    let rel = contents.lines().next()?.strip_prefix("gitdir:")?.trim();
    let gitdir = resolve_against(root, rel);

    // `<gitdir>/commondir`, if present, points at the shared common dir (usually
    // relative to the gitdir, e.g. "../.."). Absent for submodules → fall back.
    note(&gitdir.join("commondir"));
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

/// What `HEAD` resolves through, noted: `HEAD` itself, `packed-refs` and the
/// branch's loose ref. The branch, when it is on one.
fn head_inputs(dirs: &GitDirs) -> Option<String> {
    note(&dirs.commondir.join("packed-refs"));
    let branch = parse_head(dirs).0;
    if let Some(branch) = &branch {
        note(&loose_ref(dirs, "refs/heads", branch));
    }
    branch
}

/// The loose ref `<prefix>/<name>` in the common dir.
fn loose_ref(dirs: &GitDirs, prefix: &str, name: &str) -> PathBuf {
    dirs.commondir.join(prefix).join(name)
}

/// What `head` answers: read from `HEAD`, no process. A `HEAD` that cannot be
/// read is neither on a branch nor detached.
fn head_facts(dir: &Path, _: &dyn GitExecutor) -> Option<Facts> {
    let Some(dirs) = resolve_git_dir(dir) else {
        return Some(Facts::new());
    };
    let (branch, detached) = parse_head(&dirs);
    let mut facts = Facts::new();
    if let Some(branch) = branch {
        facts.insert("branch".into(), json!(branch));
    }
    facts.insert("detached".into(), json!(detached));
    Some(facts)
}

/// What `refs` answers: `None` when `git` cannot read the repository. What the
/// repo does not have is absent, never "" or 0: a commit, a tag, a push remote,
/// a stepped operation in progress. Counts (stashes) are values.
fn refs_facts(dir: &Path, executor: &dyn GitExecutor) -> Option<Facts> {
    // Outside a work tree there is nothing to report. Resolving the dirs is
    // also the guard: this source needs them anyway, so it costs one stat
    // rather than one plus a command that could only fail.
    let Some(dirs) = resolve_git_dir(dir) else {
        return Some(Facts::new());
    };

    let branch = head_inputs(&dirs);
    note_dirs_below(&dirs.commondir.join("refs/tags"));
    note(&dirs.commondir.join("config"));
    let stash_count = count_stashes(&dirs);
    let (state, steps) = detect_repo_state(&dirs);
    // Started together: the answer waits for the slowest, not for their sum.
    let (head, tag, push) = std::thread::scope(|scope| {
        let tag = scope.spawn(|| get_nearest_tag(dir, executor));
        let push = scope.spawn(|| get_push_divergence(&dirs, dir, branch.as_deref(), executor));
        let head = get_head_info(dir, executor);
        (head, tag.join().ok().flatten(), push.join().ok().flatten())
    });
    // No commit is a repository with none yet, or one git cannot read.
    if head.is_none() && !is_git_repository(dir, executor) {
        return None;
    }

    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let mut facts = Facts::new();
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

/// `dir` and every directory below it, each as a node: an entry made, removed
/// or renamed in one is a change. Git rewrites a loose ref by renaming a
/// lockfile over it, so that is every tag made, moved or deleted, however its
/// name nests.
fn note_dirs_below(dir: &Path) {
    note(dir);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            note_dirs_below(&entry.path());
        }
    }
}

/// Whether git takes `dir` for a repository: asked only of one with no commit
/// to show, so the common run pays nothing for it.
fn is_git_repository(dir: &Path, executor: &dyn GitExecutor) -> bool {
    let output = executor.run_git(dir, vec!["rev-parse".into(), "--git-dir".into()]);
    matches!(output, Ok(o) if o.status.success())
}

/// What `diff` answers.
fn diff_facts(dir: &Path, executor: &dyn GitExecutor) -> Option<Facts> {
    // `git diff --numstat` fails outside a work tree and the parse turns a
    // failure into `(0, 0)`, so without this guard every non-repository
    // directory answers with four zeros: a hit, not the miss it is.
    let Some(dirs) = resolve_git_dir(dir) else {
        return Some(Facts::new());
    };
    head_inputs(&dirs);
    note(&dirs.gitdir.join("index"));

    // Started together: the answer waits for the slower, not for their sum.
    let ((lines_added, lines_removed), (lines_staged_added, lines_staged_removed)) =
        std::thread::scope(|scope| {
            let staged = scope.spawn(|| diff_numstat_staged(dir, executor));
            let worktree = diff_numstat(dir, executor);
            (worktree, staged.join().unwrap_or((0, 0)))
        });

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
    let Some(branch) = head_inputs(&dirs) else {
        return Some(Facts::new());
    };

    let base = detect_default_branch(&dirs, dir, executor);
    let head = ref_oid(&dirs, &branch);

    // Parent: the reflog's recorded fork source when it names a local branch
    // that still exists and is not this branch; otherwise the default branch.
    let parent = read_reflog_parent(&dirs, &branch)
        .filter(|p| p != &branch && local_branch_exists(&dirs, dir, p, executor))
        .map(|p| (p.clone(), p))
        .or_else(|| base.clone());

    let parent_counts = parent
        .as_ref()
        .map(|(_, rev)| lines_since(dir, rev, head.as_deref(), executor));

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
            _ => lines_since(dir, rev, head.as_deref(), executor),
        };
        facts.insert("branch_default".into(), json!(name));
        facts.insert("lines_added_default".into(), json!(added));
        facts.insert("lines_removed_default".into(), json!(removed));
    }

    Some(facts)
}

/// What `status` answers: `None` when `git status` fails. One `git status`
/// says all of it: what the branch's upstream is, how far it has moved, and
/// what the work tree and the index hold. What the repo does not have is
/// absent: an upstream, and with it ahead and behind.
fn status_facts(dir: &Path, executor: &dyn GitExecutor) -> Option<Facts> {
    // Outside a work tree there is nothing to report: the filesystem probe
    // says so without spawning anything.
    let Some(dirs) = resolve_git_dir(dir) else {
        return Some(Facts::new());
    };
    head_inputs(&dirs);
    note(&dirs.gitdir.join("index"));
    note(&dirs.commondir.join("config"));

    let began = SystemTime::now();
    let status = parse_git_status(dir, executor)?;
    if let Some(upstream) = &status.upstream {
        note_upstream(&dirs, upstream, began);
    }

    let dirty =
        status.staged > 0 || status.unstaged > 0 || status.untracked > 0 || status.conflicted > 0;

    let mut facts = Facts::new();
    if let Some(upstream) = status.upstream {
        facts.insert("upstream".into(), json!(upstream));
        if let Some((ahead, behind)) = status.ab {
            facts.insert("ahead".into(), json!(ahead));
            facts.insert("behind".into(), json!(behind));
        }
    }
    facts.insert("staged".into(), json!(status.staged));
    facts.insert("unstaged".into(), json!(status.unstaged));
    facts.insert("untracked".into(), json!(status.untracked));
    facts.insert("conflicted".into(), json!(status.conflicted));
    facts.insert("dirty".into(), json!(dirty));
    Some(facts)
}

/// The upstream's loose ref, noted. Only `git status` says which that is, so
/// the stamp comes after the run read it, and carries when that run `began`: a
/// ref written since then is within the stamp's racy window, which fails a
/// later check, one more run, as a stamp taken first would have.
///
/// shortcut: `<remote>/<branch>` is `refs/remotes/<remote>/<branch>` under the
/// default fetch refspec; another refspec's upstream is left to the watch and
/// the poll, until one is worth asking git about.
fn note_upstream(dirs: &GitDirs, upstream: &str, began: SystemTime) {
    note_taken(
        // A local upstream reads as the bare branch name.
        ["refs/remotes", "refs/heads"].map(|prefix| Touch {
            at: began,
            ..Touch::take(&loose_ref(dirs, prefix, upstream))
        }),
    );
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
    note(&head_path);
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
    let mut upstream = None;
    let mut ab = None;
    let mut staged: i64 = 0;
    let mut unstaged: i64 = 0;
    let mut untracked: i64 = 0;
    let mut conflicted: i64 = 0;

    for line in stdout.lines() {
        if let Some(name) = line.strip_prefix("# branch.upstream ") {
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
    note(&stash_log);
    std::fs::read_to_string(&stash_log)
        .map(|s| s.lines().count() as i64)
        .unwrap_or(0)
}

/// Returns (state_name, (step, total) for a stepped operation in progress).
fn detect_repo_state(dirs: &GitDirs) -> (String, Option<(i64, i64)>) {
    let git_dir = &dirs.gitdir;
    for marker in [
        "MERGE_HEAD",
        "rebase-merge/msgnum",
        "rebase-merge/end",
        "rebase-apply/next",
        "rebase-apply/last",
        "CHERRY_PICK_HEAD",
        "BISECT_LOG",
        "REVERT_HEAD",
    ] {
        note(&git_dir.join(marker));
    }
    note_taken(["rebase-merge", "rebase-apply"].map(|dir| Touch::presence(&git_dir.join(dir))));

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
fn get_push_divergence(
    dirs: &GitDirs,
    dir: &Path,
    branch: Option<&str>,
    executor: &dyn GitExecutor,
) -> Option<(i64, i64)> {
    let branch = branch?;
    let push_remote = get_push_remote(dir, branch, executor)?;

    let refspec = format!("{push_remote}/{branch}");
    note(&loose_ref(dirs, "refs/remotes", &refspec));

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

/// The branch's push remote: `branch.<name>.pushRemote`, then
/// `remote.pushDefault`, the last value of each as git's own lookup has it. One
/// listing answers both.
fn get_push_remote(dir: &Path, branch: &str, executor: &dyn GitExecutor) -> Option<String> {
    let output = executor
        .run_git(dir, vec!["config".into(), "--list".into(), "--null".into()])
        .ok()
        .filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&output.stdout);
    // Git writes a key's section and variable in lower case, a subsection as it is.
    let own = format!("branch.{branch}.pushremote");
    let (mut remote, mut default) = (None, None);
    for entry in text.split('\0') {
        // `key\nvalue`; a key with no value has no line to give.
        let Some((key, value)) = entry.split_once('\n').filter(|(_, v)| !v.is_empty()) else {
            continue;
        };
        if key == own {
            remote = Some(value);
        } else if key == "remote.pushdefault" {
            default = Some(value);
        }
    }
    remote.or(default).map(str::to_string)
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
    note(&reflog);
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
    note(&origin_head);
    if let Ok(contents) = std::fs::read_to_string(&origin_head)
        && let Some(name) = contents
            .trim()
            .strip_prefix("ref: refs/remotes/origin/")
            .filter(|n| !n.is_empty())
    {
        note(&loose_ref(dirs, "refs/remotes/origin", name));
        return Some((name.to_string(), format!("origin/{name}")));
    }

    ["main", "master"]
        .into_iter()
        .find(|name| local_branch_exists(dirs, dir, name, executor))
        .map(|name| (name.to_string(), name.to_string()))
}

/// Whether `refs/heads/<branch>` resolves: its loose ref says so with no
/// process. Without one git is asked, so packed refs — the normal state of a
/// fresh clone — count.
fn local_branch_exists(
    dirs: &GitDirs,
    dir: &Path,
    branch: &str,
    executor: &dyn GitExecutor,
) -> bool {
    let loose = loose_ref(dirs, "refs/heads", branch);
    note(&loose);
    if loose.is_file() {
        return true;
    }
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
///
/// `head` is the commit HEAD is at, when the files say: what two commits differ
/// by never changes, and it is the dearest thing this Source does, so it is
/// kept. A ref rewritten with what it held (`git stash`, `git reset`) or a
/// checkout back to a branch costs it nothing.
fn lines_since(
    dir: &Path,
    rev: &str,
    head: Option<&str>,
    executor: &dyn GitExecutor,
) -> (i64, i64) {
    let Some(point) = merge_base(dir, rev, executor) else {
        return (0, 0);
    };
    let key = head.map(|head| (point.clone(), head.to_string()));
    if let Some(counts) = key
        .as_ref()
        .and_then(|key| lock_compared().get(key).copied())
    {
        return counts;
    }
    let output = executor.run_git(
        dir,
        vec!["diff".into(), "--numstat".into(), format!("{point}..HEAD")],
    );
    let ok = matches!(&output, Ok(o) if o.status.success());
    let counts = parse_numstat_output(output);
    if let (true, Some(key)) = (ok, key) {
        let mut compared = lock_compared();
        if compared.len() >= MOST_COMPARED {
            compared.clear();
        }
        compared.insert(key, counts);
    }
    counts
}

/// Lines added and removed between two commits, by `(point, head)`.
type Compared = HashMap<(String, String), (i64, i64)>;

/// The most comparisons kept: a few branches' worth of recent commits.
const MOST_COMPARED: usize = 64;

/// What `git diff --numstat <point>..<head>` said, by `(point, head)`.
///
/// shortcut: a change to `diff.renames` is not seen until the daemon restarts;
/// keep the setting in the key if a repository's ever needs to count.
fn lock_compared() -> std::sync::MutexGuard<'static, Compared> {
    static COMPARED: LazyLock<Mutex<Compared>> = LazyLock::new(Mutex::default);
    COMPARED.lock().unwrap_or_else(|e| e.into_inner())
}

/// The commit `refs/heads/<branch>` is at: its loose ref, else `packed-refs`.
fn ref_oid(dirs: &GitDirs, branch: &str) -> Option<String> {
    let oid = |text: &str| Some(text.trim().to_string()).filter(|t| is_hex_oid(t));
    if let Ok(text) = std::fs::read_to_string(loose_ref(dirs, "refs/heads", branch)) {
        return oid(&text);
    }
    let packed = std::fs::read_to_string(dirs.commondir.join("packed-refs")).ok()?;
    let name = format!("refs/heads/{branch}");
    packed
        .lines()
        .find_map(|line| line.split_once(' ').filter(|(_, r)| *r == name))
        .and_then(|(hash, _)| oid(hash))
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
        assert_eq!(names, ["diff", "head", "lineage", "refs", "status"]);
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
