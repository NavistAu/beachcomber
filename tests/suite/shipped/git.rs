//! `defaults/git.toml`: five Sources over the compiled-in `git` plugin, `head`
//! reading two files itself. The Sources are run from the shipped file as the
//! daemon runs them, on `GitRepoFixture` repositories; what a read does with
//! them is read through a daemon below.

use super::{age, scenario, schedule, served};
use crate::common::fetch::fetch;
use crate::common::git::GitRepoFixture;
use crate::common::poll::{poll_until, poll_until_async, poll_until_eq};
use beachcomber::cache::Cache;
use beachcomber::client::Client;
use beachcomber::config::Config;
use beachcomber::provider::{InvalidationStrategy, Source, SourceMetadata};
use beachcomber::scheduler::Scheduler;
use beachcomber::server::Server;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

/// The shipped file's Sources, in order.
fn git_sources() -> Vec<Arc<dyn Source>> {
    let registry = crate::common::registry::with_shipped_defaults();
    let names: Vec<String> = git_meta().into_iter().map(|s| s.name).collect();
    names
        .iter()
        .map(|name| registry.source("git", name).expect("a shipped Source"))
        .collect()
}

fn git_meta() -> Vec<SourceMetadata> {
    crate::common::registry::with_shipped_defaults()
        .provider_sources("git")
        .expect("git is shipped")
        .to_vec()
}

#[test]
fn git_root_returns_repo_root_from_subdir() {
    let repo = GitRepoFixture::new();
    let subdir = repo.create_subdir("src/lib");

    let sources = git_sources();
    let got = sources[0].root(subdir.to_str().unwrap()).unwrap();
    let expected = repo.path().canonicalize().unwrap();
    assert_eq!(got, Some(expected.to_string_lossy().to_string()));
}

#[test]
fn git_root_returns_repo_root_when_called_at_root() {
    let repo = GitRepoFixture::new();
    let sources = git_sources();
    let got = sources[0].root(repo.path_str()).unwrap();
    let expected = repo.path().canonicalize().unwrap();
    assert_eq!(got, Some(expected.to_string_lossy().to_string()));
}

#[test]
fn git_root_returns_none_outside_any_repo() {
    let tmp = TempDir::new().unwrap();
    let sources = git_sources();
    let got = sources[0].root(tmp.path().to_str().unwrap()).unwrap();
    if let Some(got) = got {
        assert_ne!(
            got,
            tmp.path()
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .to_string(),
            "tempdir has no .git; root should not return the dir itself"
        );
    }
}

#[test]
fn git_provider_metadata() {
    let meta = git_meta();
    assert_eq!(meta.len(), 5);

    let head_src = meta.iter().find(|s| s.name == "head").unwrap();
    let head_fields: Vec<&str> = head_src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(head_fields.contains(&"branch"));
    assert!(head_fields.contains(&"detached"));

    let refs_src = meta.iter().find(|s| s.name == "refs").unwrap();
    let refs_fields: Vec<&str> = refs_src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(
        !refs_fields.contains(&"branch"),
        "branch moved to head source"
    );
    assert!(
        !refs_fields.contains(&"detached"),
        "detached moved to head source"
    );
    assert!(refs_fields.contains(&"ahead"));
    assert!(refs_fields.contains(&"behind"));
    assert!(refs_fields.contains(&"upstream"));
    assert!(refs_fields.contains(&"commit"));
    assert!(refs_fields.contains(&"tag"));
    assert!(refs_fields.contains(&"stash"));
    assert!(refs_fields.contains(&"state"));
    assert!(refs_fields.contains(&"state_step"));
    assert!(refs_fields.contains(&"state_total"));
    assert!(refs_fields.contains(&"last_commit_age_secs"));
    assert!(refs_fields.contains(&"commit_summary"));
    assert!(refs_fields.contains(&"push_ahead"));
    assert!(refs_fields.contains(&"push_behind"));

    let diff_src = meta.iter().find(|s| s.name == "diff").unwrap();
    let diff_fields: Vec<&str> = diff_src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(diff_fields.contains(&"lines_added"));
    assert!(diff_fields.contains(&"lines_removed"));
    assert!(diff_fields.contains(&"lines_staged_added"));
    assert!(diff_fields.contains(&"lines_staged_removed"));

    let lineage_src = meta.iter().find(|s| s.name == "lineage").unwrap();
    let lineage_fields: Vec<&str> = lineage_src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(lineage_fields.contains(&"branch_parent"));
    assert!(lineage_fields.contains(&"lines_added_parent"));
    assert!(lineage_fields.contains(&"lines_removed_parent"));
    assert!(lineage_fields.contains(&"branch_default"));
    assert!(lineage_fields.contains(&"lines_added_default"));
    assert!(lineage_fields.contains(&"lines_removed_default"));

    let status_src = meta.iter().find(|s| s.name == "status").unwrap();
    let status_fields: Vec<&str> = status_src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(status_fields.contains(&"staged"));
    assert!(status_fields.contains(&"unstaged"));
    assert!(status_fields.contains(&"untracked"));
    assert!(status_fields.contains(&"conflicted"));
    assert!(status_fields.contains(&"dirty"));
}

#[test]
fn git_provider_source_names() {
    let sources = git_sources();
    assert_eq!(sources.len(), 5);
    let names: Vec<&str> = sources.iter().map(|s| s.metadata().name.as_str()).collect();
    assert!(names.contains(&"head"));
    assert!(names.contains(&"refs"));
    assert!(names.contains(&"diff"));
    assert!(names.contains(&"lineage"));
    assert!(names.contains(&"status"));
}

#[test]
fn git_refs_returns_empty_for_non_repo() {
    let tmp = TempDir::new().unwrap();
    let sources = git_sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src
        .execute(Some(tmp.path().to_str().unwrap()))
        .unwrap();
    assert!(
        result.fields.is_empty(),
        "Non-git directory should return empty SourceResult"
    );
}

#[test]
fn git_refs_returns_branch() {
    // branch and detached moved to the head source; verify head reports the branch.
    let repo = GitRepoFixture::new();
    let sources = git_sources();
    let head_src = sources
        .iter()
        .find(|s| s.metadata().name == "head")
        .unwrap();
    let result = head_src.execute(Some(repo.path_str())).unwrap();
    let branch = result.fields.get("branch").unwrap().as_text();
    assert!(!branch.is_empty(), "Branch should not be empty");
}

#[test]
fn git_status_clean_repo() {
    let repo = GitRepoFixture::new();
    let sources = git_sources();
    let status_src = sources
        .iter()
        .find(|s| s.metadata().name == "status")
        .unwrap();
    let result = status_src.execute(Some(repo.path_str())).unwrap();
    assert_eq!(result.fields.get("dirty").unwrap().as_text(), "false");
    assert_eq!(result.fields.get("staged").unwrap().as_text(), "0");
    assert_eq!(result.fields.get("unstaged").unwrap().as_text(), "0");
    assert_eq!(result.fields.get("untracked").unwrap().as_text(), "0");
}

#[test]
fn git_status_dirty_repo() {
    let repo = GitRepoFixture::new().with_untracked_file("new_file.txt", "content");
    let sources = git_sources();
    let status_src = sources
        .iter()
        .find(|s| s.metadata().name == "status")
        .unwrap();
    let result = status_src.execute(Some(repo.path_str())).unwrap();
    assert_eq!(result.fields.get("dirty").unwrap().as_text(), "true");
    assert_eq!(result.fields.get("untracked").unwrap().as_text(), "1");
}

#[test]
fn git_status_staged_changes() {
    let repo = GitRepoFixture::new().with_staged_file("staged.txt", "content");
    let sources = git_sources();
    let status_src = sources
        .iter()
        .find(|s| s.metadata().name == "status")
        .unwrap();
    let result = status_src.execute(Some(repo.path_str())).unwrap();
    assert_eq!(result.fields.get("staged").unwrap().as_text(), "1");
}

#[test]
fn git_status_unstaged_changes() {
    let repo = GitRepoFixture::new().with_unstaged_change("README.md", "modified");
    let sources = git_sources();
    let status_src = sources
        .iter()
        .find(|s| s.metadata().name == "status")
        .unwrap();
    let result = status_src.execute(Some(repo.path_str())).unwrap();
    assert_eq!(result.fields.get("unstaged").unwrap().as_text(), "1");
}

#[test]
fn git_refs_stash_count() {
    let repo = GitRepoFixture::new()
        .with_unstaged_change("README.md", "stash me")
        .with_stash();
    let sources = git_sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(repo.path_str())).unwrap();
    assert_eq!(result.fields.get("stash").unwrap().as_text(), "1");
}

#[test]
fn git_refs_clean_repo_new_fields() {
    let repo = GitRepoFixture::new();
    let sources = git_sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(repo.path_str())).unwrap();

    // No upstream in a local-only repo: absent, as are ahead and behind.
    assert!(
        !result.fields.contains_key("upstream"),
        "{:?}",
        result.fields
    );
    assert!(!result.fields.contains_key("ahead"), "{:?}", result.fields);
    // No stepped operation in a clean repo: no step, no total.
    assert!(
        !result.fields.contains_key("state_step"),
        "{:?}",
        result.fields
    );
    assert!(
        !result.fields.contains_key("state_total"),
        "{:?}",
        result.fields
    );
    // state is clean
    assert_eq!(result.fields.get("state").unwrap().as_text(), "clean");

    // detached is now owned by the head source
    let head_src = sources
        .iter()
        .find(|s| s.metadata().name == "head")
        .unwrap();
    let head_result = head_src.execute(Some(repo.path_str())).unwrap();
    // HEAD is not detached after a normal commit
    assert_eq!(
        head_result.fields.get("detached").unwrap().as_text(),
        "false"
    );
}

#[test]
fn git_refs_commit_hash_format() {
    let repo = GitRepoFixture::new();
    let sources = git_sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(repo.path_str())).unwrap();
    let commit = result.fields.get("commit").unwrap().as_text();
    // Short SHA: non-empty, all hex, typically 7 chars
    assert!(!commit.is_empty(), "commit should not be empty");
    assert!(
        commit.chars().all(|c| c.is_ascii_hexdigit()),
        "commit should be hex: {commit}"
    );
    assert!(
        commit.len() >= 4 && commit.len() <= 40,
        "unexpected commit length: {}",
        commit.len()
    );
}

#[test]
fn git_refs_last_commit_age_secs() {
    let repo = GitRepoFixture::new();
    let sources = git_sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(repo.path_str())).unwrap();
    let age: i64 = result
        .fields
        .get("last_commit_age_secs")
        .unwrap()
        .as_text()
        .parse()
        .unwrap();
    assert!(age >= 0, "age should be non-negative");
    assert!(age < 60, "last_commit_age_secs should be recent: {age}");
}

#[test]
fn git_diff_lines_added_removed_unstaged() {
    // README.md has "# test" (1 line). Replace with 3 lines.
    let repo = GitRepoFixture::new().with_unstaged_change("README.md", "line1\nline2\nline3");
    let sources = git_sources();
    let diff_src = sources
        .iter()
        .find(|s| s.metadata().name == "diff")
        .unwrap();
    let result = diff_src.execute(Some(repo.path_str())).unwrap();
    let added: i64 = result
        .fields
        .get("lines_added")
        .unwrap()
        .as_text()
        .parse()
        .unwrap();
    let removed: i64 = result
        .fields
        .get("lines_removed")
        .unwrap()
        .as_text()
        .parse()
        .unwrap();
    assert!(added > 0, "lines_added should be > 0, got {added}");
    assert!(removed > 0, "lines_removed should be > 0, got {removed}");
}

#[test]
fn git_diff_lines_staged_added_removed() {
    // Stage an addition of a new file with 2 lines
    let repo = GitRepoFixture::new().with_staged_file("new.txt", "alpha\nbeta");
    let sources = git_sources();
    let diff_src = sources
        .iter()
        .find(|s| s.metadata().name == "diff")
        .unwrap();
    let result = diff_src.execute(Some(repo.path_str())).unwrap();
    let staged_added: i64 = result
        .fields
        .get("lines_staged_added")
        .unwrap()
        .as_text()
        .parse()
        .unwrap();
    let staged_removed: i64 = result
        .fields
        .get("lines_staged_removed")
        .unwrap()
        .as_text()
        .parse()
        .unwrap();
    assert_eq!(
        staged_added, 2,
        "staged_added should be 2, got {staged_added}"
    );
    assert_eq!(
        staged_removed, 0,
        "staged_removed should be 0 for a new file, got {staged_removed}"
    );
}

#[test]
fn git_refs_commit_summary() {
    let repo = GitRepoFixture::new();
    let sources = git_sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(repo.path_str())).unwrap();
    let summary = result.fields.get("commit_summary").unwrap().as_text();
    assert_eq!(
        summary, "init",
        "commit_summary should be the first commit message"
    );
}

#[test]
fn git_refs_push_ahead_behind_no_push_remote() {
    let repo = GitRepoFixture::new();
    let sources = git_sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(repo.path_str())).unwrap();
    // No push remote configured: nothing to diverge from, so both are absent.
    assert!(
        !result.fields.contains_key("push_ahead"),
        "{:?}",
        result.fields
    );
    assert!(
        !result.fields.contains_key("push_behind"),
        "{:?}",
        result.fields
    );
}

#[test]
fn git_refs_detached_head() {
    // detached is now owned by the head source.
    let repo = GitRepoFixture::new().with_detached_head();

    let sources = git_sources();
    let head_src = sources
        .iter()
        .find(|s| s.metadata().name == "head")
        .unwrap();
    let result = head_src.execute(Some(repo.path_str())).unwrap();
    assert_eq!(result.fields.get("detached").unwrap().as_text(), "true");
}

#[test]
fn git_diff_clean_repo_has_zero_lines() {
    let repo = GitRepoFixture::new();
    let sources = git_sources();
    let diff_src = sources
        .iter()
        .find(|s| s.metadata().name == "diff")
        .unwrap();
    let result = diff_src.execute(Some(repo.path_str())).unwrap();
    assert_eq!(result.fields.get("lines_added").unwrap().as_text(), "0");
    assert_eq!(result.fields.get("lines_removed").unwrap().as_text(), "0");
    assert_eq!(
        result.fields.get("lines_staged_added").unwrap().as_text(),
        "0"
    );
    assert_eq!(
        result.fields.get("lines_staged_removed").unwrap().as_text(),
        "0"
    );
}

#[test]
fn git_sibling_sources_have_disjoint_fields() {
    // refs, diff, and status should not share any field names
    let meta = git_meta();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for src in &meta {
        for f in &src.fields {
            assert!(
                seen.insert(f.name.clone()),
                "field '{}' appears in multiple sources",
                f.name
            );
        }
    }
}

#[test]
fn git_refs_stash_counted_in_linked_worktree() {
    let repo = GitRepoFixture::new();
    let wt = repo.add_worktree("wt", "feature");

    // Dirty the worktree's copy of README.md, then stash it from the worktree.
    std::fs::write(wt.join("README.md"), "stash me in the worktree").unwrap();
    repo.git_in(&wt, &["stash"]);

    let sources = git_sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(wt.to_str().unwrap())).unwrap();

    // The stash reflog lives in the SHARED commondir, not <wt>/.git (a file).
    assert_eq!(
        result.fields.get("stash").unwrap().as_text(),
        "1",
        "stash created in a linked worktree must be counted via commondir"
    );
}

#[test]
fn git_refs_state_detected_in_linked_worktree() {
    let repo = GitRepoFixture::new();
    let wt = repo.add_worktree("wt", "feature");

    // Build a REAL conflicting merge so MERGE_HEAD is guaranteed to be written.
    //
    // From the worktree on `feature`:
    //   1. Create branch `other`, write "A" to README.md, commit.
    //   2. Back on `feature`, write "B" to README.md, commit.
    //   3. `git merge other` — this WILL conflict (exit non-zero). That is
    //      expected and correct; the conflict is what writes MERGE_HEAD.
    //
    // `git_in_allow_failure` is used for step 3 because git exits non-zero
    // on a conflict. All other calls use the normal `git_in` which panics on failure.

    // 1. branch `other` with "A"
    repo.git_in(&wt, &["checkout", "-b", "other"]);
    std::fs::write(wt.join("README.md"), "A").expect("write README.md A");
    repo.git_in(&wt, &["add", "README.md"]);
    repo.git_in(&wt, &["commit", "-m", "other: set A"]);

    // 2. back on `feature` with "B"
    repo.git_in(&wt, &["checkout", "feature"]);
    std::fs::write(wt.join("README.md"), "B").expect("write README.md B");
    repo.git_in(&wt, &["add", "README.md"]);
    repo.git_in(&wt, &["commit", "-m", "feature: set B"]);

    // 3. conflicting merge — exit non-zero is EXPECTED (conflict writes MERGE_HEAD)
    repo.git_in_allow_failure(&wt, &["merge", "other"]);

    let sources = git_sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(wt.to_str().unwrap())).unwrap();

    // MERGE_HEAD lives under <main>/.git/worktrees/wt, not <wt>/.git (a file).
    assert_eq!(
        result.fields.get("state").unwrap().as_text(),
        "merge",
        "in-progress merge in a linked worktree must be detected via the resolved gitdir"
    );
}

// ── lineage source ────────────────────────────────────────────────────────────

/// Build `main → devel (+3 lines) → feature (+2 lines)`, with `devel` and
/// `feature` each created from an explicit start point so their reflogs record
/// `branch: Created from <start>` rather than `HEAD`. The fixture's repository
/// starts on `main` (`git init --initial-branch=main`).
fn lineage_fixture() -> GitRepoFixture {
    let repo = GitRepoFixture::new();
    repo.with_branch_from("devel", "main")
        .with_staged_file("f.txt", "a\nb\nc\n")
        .with_commit("devel work")
        .with_branch_from("feature", "devel")
        .with_staged_file("g.txt", "x\ny\n")
        .with_commit("feature work")
}

fn lineage_fields(path: &str) -> std::collections::HashMap<String, String> {
    let sources = git_sources();
    let src = sources
        .iter()
        .find(|s| s.metadata().name == "lineage")
        .unwrap();
    src.execute(Some(path))
        .unwrap()
        .fields
        .iter()
        .map(|(k, v)| (k.clone(), v.as_text()))
        .collect()
}

#[test]
fn git_lineage_parent_from_reflog_and_counts_match_known_diff() {
    let repo = lineage_fixture();
    let got = lineage_fields(repo.path_str());

    assert_eq!(
        got.get("branch_parent").map(String::as_str),
        Some("devel"),
        "reflog records `Created from devel`; got {got:?}"
    );
    // feature adds g.txt (2 lines) on top of devel.
    assert_eq!(got.get("lines_added_parent").map(String::as_str), Some("2"));
    assert_eq!(
        got.get("lines_removed_parent").map(String::as_str),
        Some("0")
    );

    assert_eq!(got.get("branch_default").map(String::as_str), Some("main"));
    // Against main, feature carries devel's f.txt (3 lines) plus g.txt (2).
    assert_eq!(
        got.get("lines_added_default").map(String::as_str),
        Some("5")
    );
    assert_eq!(
        got.get("lines_removed_default").map(String::as_str),
        Some("0")
    );
}

#[test]
fn git_lineage_counts_removed_lines() {
    let repo = lineage_fixture();
    // Delete two of f.txt's three lines and commit on feature.
    let repo = repo
        .with_staged_file("f.txt", "a\n")
        .with_commit("trim f.txt");
    let got = lineage_fields(repo.path_str());

    assert_eq!(got.get("lines_added_parent").map(String::as_str), Some("2"));
    assert_eq!(
        got.get("lines_removed_parent").map(String::as_str),
        Some("2"),
        "two lines removed from f.txt since devel; got {got:?}"
    );
}

#[test]
fn git_lineage_missing_reflog_falls_back_to_default_branch() {
    let repo = lineage_fixture();
    // Simulate a pruned/absent reflog for the checked-out branch.
    std::fs::remove_file(repo.path().join(".git/logs/refs/heads/feature"))
        .expect("remove feature reflog");

    let got = lineage_fields(repo.path_str());
    assert_eq!(
        got.get("branch_parent").map(String::as_str),
        Some("main"),
        "with no reflog the parent falls back to the default branch; got {got:?}"
    );
    assert_eq!(got.get("branch_default").map(String::as_str), Some("main"));
    // Falling back means both comparisons use the same rev.
    assert_eq!(got.get("lines_added_parent").map(String::as_str), Some("5"));
    assert_eq!(
        got.get("lines_added_default").map(String::as_str),
        Some("5")
    );
}

#[test]
fn git_lineage_deleted_parent_falls_back_to_default_branch() {
    let repo = lineage_fixture();
    // `devel` is still named by feature's reflog, but the branch is gone.
    repo.git_in(repo.path(), &["branch", "-D", "devel"]);

    let got = lineage_fields(repo.path_str());
    assert_eq!(
        got.get("branch_parent").map(String::as_str),
        Some("main"),
        "a deleted parent falls back to the default branch; got {got:?}"
    );
    assert_eq!(got.get("lines_added_parent").map(String::as_str), Some("5"));
}

#[test]
fn git_lineage_created_from_head_falls_back_to_default_branch() {
    // `git checkout -b <new>` with no start point records `Created from HEAD`,
    // which names no branch.
    let repo = GitRepoFixture::new();
    let repo = repo
        .with_branch("solo")
        .with_staged_file("g.txt", "x\ny\n")
        .with_commit("solo work");

    let got = lineage_fields(repo.path_str());
    assert_eq!(got.get("branch_parent").map(String::as_str), Some("main"));
    assert_eq!(got.get("lines_added_parent").map(String::as_str), Some("2"));
}

#[test]
fn git_lineage_on_default_branch_without_remote_reports_zero() {
    let repo = GitRepoFixture::new();

    let got = lineage_fields(repo.path_str());
    // No origin/HEAD, so the default branch resolves to the local `main` — which
    // is HEAD. Deterministically zero rather than absent.
    assert_eq!(got.get("branch_default").map(String::as_str), Some("main"));
    assert_eq!(
        got.get("lines_added_default").map(String::as_str),
        Some("0")
    );
    assert_eq!(
        got.get("lines_removed_default").map(String::as_str),
        Some("0")
    );
    assert_eq!(got.get("branch_parent").map(String::as_str), Some("main"));
    assert_eq!(got.get("lines_added_parent").map(String::as_str), Some("0"));
}

#[test]
fn git_lineage_default_branch_from_origin_head_measures_unpushed_work() {
    let repo = GitRepoFixture::new();
    // Stand up what a clone would have: a remote-tracking ref plus the symref
    // `refs/remotes/origin/HEAD -> refs/remotes/origin/main`.
    repo.git_in(
        repo.path(),
        &["update-ref", "refs/remotes/origin/main", "main"],
    );
    repo.git_in(
        repo.path(),
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    // Commit locally, ahead of origin/main.
    let repo = repo
        .with_staged_file("g.txt", "x\ny\n")
        .with_commit("unpushed work");

    let got = lineage_fields(repo.path_str());
    assert_eq!(
        got.get("branch_default").map(String::as_str),
        Some("main"),
        "base is the branch name from origin/HEAD; got {got:?}"
    );
    assert_eq!(
        got.get("lines_added_default").map(String::as_str),
        Some("2"),
        "on the default branch the counts measure work not yet on origin"
    );
}

#[test]
fn git_lineage_detached_head_emits_nothing() {
    let repo = lineage_fixture().with_detached_head();
    let got = lineage_fields(repo.path_str());
    assert!(
        got.is_empty(),
        "detached HEAD has no branch, so no lineage fields; got {got:?}"
    );
}

#[test]
fn git_lineage_returns_empty_for_non_repo() {
    let tmp = TempDir::new().unwrap();
    let got = lineage_fields(tmp.path().to_str().unwrap());
    assert!(got.is_empty());
}

#[test]
fn git_lineage_parent_resolved_in_linked_worktree() {
    let repo = lineage_fixture();
    // Branch reflogs live in the shared commondir, not <wt>/.git (a file), so a
    // linked worktree must resolve its parent through commondir.
    repo.git_in(
        repo.path(),
        &["worktree", "add", "wt", "-b", "wt-feature", "devel"],
    );
    let wt = repo.path().join("wt");

    let got = lineage_fields(wt.to_str().unwrap());
    assert_eq!(
        got.get("branch_parent").map(String::as_str),
        Some("devel"),
        "worktree branch reflog must be read from the commondir; got {got:?}"
    );
    assert_eq!(
        got.get("lines_added_parent").map(String::as_str),
        Some("0"),
        "wt-feature has no commits of its own yet"
    );
    assert_eq!(got.get("branch_default").map(String::as_str), Some("main"));
    assert_eq!(
        got.get("lines_added_default").map(String::as_str),
        Some("3")
    );
}

#[test]
fn git_head_source_reports_branch() {
    let repo = GitRepoFixture::new();
    let sources = git_sources();
    let head_src = sources
        .iter()
        .find(|s| s.metadata().name == "head")
        .unwrap();

    let result = head_src.execute(Some(repo.path_str())).unwrap();
    let branch = result.fields.get("branch").unwrap().as_text();
    assert!(!branch.is_empty(), "head source must report branch name");
    assert_eq!(
        result.fields.get("detached").unwrap().as_text(),
        "false",
        "head source must report detached=false on a normal checkout"
    );
}

#[test]
fn git_head_branch_correct_in_linked_worktree() {
    let repo = GitRepoFixture::new();
    // add_worktree creates a new branch "wt-branch" checked out in the linked worktree.
    let wt = repo.add_worktree("my-wt", "wt-branch");

    let sources = git_sources();
    let head_src = sources
        .iter()
        .find(|s| s.metadata().name == "head")
        .unwrap();

    let result = head_src.execute(Some(wt.to_str().unwrap())).unwrap();
    let branch = result.fields.get("branch").unwrap().as_text();
    assert_eq!(
        branch, "wt-branch",
        "head source must read the correct branch for a linked worktree via resolve_git_dir"
    );
    assert_eq!(
        result.fields.get("detached").unwrap().as_text(),
        "false",
        "linked worktree on a branch must not be detached"
    );
}

// ── through a daemon ──────────────────────────────────────────────────────────

async fn setup_daemon() -> (
    TempDir,
    std::path::PathBuf,
    beachcomber::scheduler::SchedulerHandle,
) {
    let tmp = TempDir::new().unwrap();
    let sock = tmp.path().join("sock");
    let watchers = Arc::new(beachcomber::watcher_registry::WatcherRegistry::new());
    let cache = Arc::new(Cache::with_watchers(watchers.clone()));
    let registry = Arc::new(crate::common::registry::with_shipped_defaults());
    let config = Config::default();

    let (handle, mut scheduler) =
        Scheduler::new(cache.clone(), registry.clone(), config, watchers.clone());
    // These tests are about git data, not the watch backend: a kernel FSEvents
    // registration costs ~1.2s per watch() on macOS and the runtime drop waits
    // for it. `SchedulerHandle::send` buffers until `run()` receives, so there
    // is nothing to wait for after spawning.
    scheduler.use_polling_watcher(Duration::from_millis(50));
    tokio::spawn(async move { scheduler.run().await });

    let server = Server::new(
        sock.clone(),
        cache,
        registry,
        Some(handle.clone()),
        watchers,
    );
    // `spawn` binds the socket synchronously — it is connectable on return.
    server.spawn().expect("server binds socket");

    (tmp, sock, handle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_git_provider() {
    let repo = GitRepoFixture::new();
    let (_tmp, sock, _handle) = setup_daemon().await;
    let client = Client::new(sock.clone());

    let resp = client.refresh("git", Some(repo.path_str())).unwrap();
    assert!(resp.ok);

    let data = poll_until(
        "git.branch after refresh",
        "a get response carrying a string branch",
        Duration::from_secs(5),
        Duration::from_millis(20),
        || {
            fetch(&sock, "git.branch", Some(repo.path_str()))
                .unwrap()
                .and_then(|got| got.data)
        },
        |d| d.as_ref().is_some_and(|d| d.is_string()),
    );
    let branch = data.expect("Should get git branch");
    assert!(branch.is_string(), "Branch should be a string");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_detects_dirty_state_via_refresh() {
    let repo = GitRepoFixture::new();
    let (_tmp, sock, _handle) = setup_daemon().await;
    let client = Client::new(sock.clone());
    let repo_path = repo.path_str();

    // First refresh: clean state
    client.refresh("git", Some(repo_path)).unwrap();
    poll_until_eq(
        "git.dirty after refresh (clean repo)",
        Duration::from_secs(5),
        Duration::from_millis(20),
        || {
            fetch(&sock, "git.dirty", Some(repo_path))
                .unwrap()
                .and_then(|got| got.data)
        },
        Some(serde_json::json!(false)),
    );

    // Create a dirty file
    std::fs::write(repo.path().join("dirty.txt"), "dirty").unwrap();

    // Refresh again
    client.refresh("git", Some(repo_path)).unwrap();
    poll_until_eq(
        "git.dirty after refresh (dirty repo)",
        Duration::from_secs(5),
        Duration::from_millis(20),
        || {
            fetch(&sock, "git.dirty", Some(repo_path))
                .unwrap()
                .and_then(|got| got.data)
        },
        Some(serde_json::json!(true)),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_returns_miss_for_non_repo() {
    let tmp = TempDir::new().unwrap();
    let (_daemon_tmp, sock, handle) = setup_daemon().await;
    let client = Client::new(sock.clone());

    client
        .refresh("git", Some(tmp.path().to_str().unwrap()))
        .unwrap();

    // The refresh is asynchronous: the scheduler fans it out to every git
    // source. The scheduler handles its messages in order, so by the time it
    // answers a status request the refresh has started, and the refresh is done
    // once nothing is in flight. A non-repo path makes every source report
    // nothing, which is a result and not a failure.
    poll_until_async(
        "the scheduler's in-flight sources after refreshing git on a non-repo",
        "none in flight, i.e. the refresh finished",
        Duration::from_secs(5),
        Duration::from_millis(20),
        async || handle.get_status().await.expect("scheduler runs").in_flight,
        |in_flight| in_flight.is_empty(),
    )
    .await;
    assert!(
        handle.get_failure_states().await.is_empty(),
        "a directory with nothing to report is not a failing source"
    );

    let data = fetch(&sock, "git.branch", Some(tmp.path().to_str().unwrap()))
        .unwrap()
        .and_then(|got| got.data);
    assert!(data.is_none(), "Non-git dir should return miss");
}

// ── the file itself ───────────────────────────────────────────────────────────

/// The schedules are the Rust provider's, Source for Source.
#[test]
fn git_schedules_are_as_they_were() {
    let watch = |patterns: &[&str]| InvalidationStrategy::Watch {
        patterns: patterns.iter().map(|p| p.to_string()).collect(),
        abs_paths: vec![],
    };
    for (source, invalidation, failback) in [
        ("head", watch(&[".git"]), 60),
        ("refs", watch(&[".git"]), 60),
        (
            "diff",
            InvalidationStrategy::Poll {
                interval_secs: 30,
                count: 4,
            },
            60,
        ),
        (
            "lineage",
            InvalidationStrategy::Poll {
                interval_secs: 60,
                count: 2,
            },
            60,
        ),
        (
            "status",
            InvalidationStrategy::WatchAndPoll {
                patterns: vec![".git/index".into()],
                abs_paths: vec![],
                interval_secs: 60,
                count: 2,
            },
            30,
        ),
    ] {
        let (meta, rooted) = schedule("git", source);
        assert_eq!(meta.invalidation, invalidation, "{source}");
        assert_eq!(meta.failback.reattempts, 3, "{source}");
        assert_eq!(meta.failback.interval_secs, failback, "{source}");
        assert!(meta.nearest, "{source} is nearest");
        assert!(rooted, "{source} has a root");
    }
}

/// A read of the whole provider through `comb get`, as a user makes it: every
/// Source's fields with their types, and what this repository lacks absent.
#[test]
fn git_get_answers_every_field_with_its_type() {
    let s = scenario();
    let repo = GitRepoFixture::new().with_staged_file("new.txt", "a\nb\n");
    let got = s.get("git", repo.path()).expect("git answers");
    assert_eq!(got["branch"], json!("main"));
    assert_eq!(got["detached"], json!(false));
    assert_eq!(got["state"], json!("clean"));
    assert_eq!(got["stash"], json!(0));
    assert_eq!(got["commit_summary"], json!("init"));
    assert!(got["commit"].is_string(), "{got}");
    assert!(got["last_commit_age_secs"].is_i64(), "{got}");
    assert_eq!(got["lines_added"], json!(0));
    assert_eq!(got["lines_staged_added"], json!(2));
    assert_eq!(got["staged"], json!(1));
    assert_eq!(got["dirty"], json!(true));
    assert_eq!(got["branch_default"], json!("main"));
    assert_eq!(got["lines_added_default"], json!(0));
    for absent in ["upstream", "ahead", "tag", "state_step", "push_ahead"] {
        assert!(got.get(absent).is_none(), "{absent}: {got}");
    }
}

/// A free-text value reaches the record as it is: no quoting on the way.
#[test]
fn git_commit_summary_is_free_text() {
    let summary = r#"it's "quoted" = {{ not }} $(a) \ template"#;
    let repo = GitRepoFixture::new()
        .with_staged_file("f", "x")
        .with_commit(summary);
    let sources = git_sources();
    let refs = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let got = refs.execute(Some(repo.path_str())).unwrap();
    assert_eq!(got.fields.get("commit_summary").unwrap().as_text(), summary);
}

/// A repository with no commits has a branch and no commit.
#[test]
fn git_a_repository_with_no_commits() {
    let tmp = TempDir::new().unwrap();
    crate::common::git::git(tmp.path(), &["init", "--initial-branch=trunk"]);
    let path = tmp.path().to_str().unwrap();
    let sources = git_sources();
    let run = |name: &str| {
        let source = sources.iter().find(|s| s.metadata().name == name).unwrap();
        source.execute(Some(path)).unwrap().fields
    };
    let head = run("head");
    assert_eq!(head.get("branch").unwrap().as_text(), "trunk");
    assert_eq!(head.get("detached").unwrap().as_text(), "false");
    let refs = run("refs");
    assert_eq!(refs.get("state").unwrap().as_text(), "clean");
    assert!(!refs.contains_key("commit"), "{refs:?}");
    assert_eq!(run("status").get("dirty").unwrap().as_text(), "false");
    assert_eq!(run("diff").get("lines_added").unwrap().as_text(), "0");
}

/// A `.git` directory git will not read: `head` says what the file does not
/// (no branch, not detached), and a Source that needs `git status` fails, to
/// its failback, with the plugin's error.
#[test]
fn git_a_broken_repository_fails_what_needs_git() {
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir(tmp.path().join(".git")).unwrap();
    let path = tmp.path().to_str().unwrap();
    let sources = git_sources();
    let find = |name: &str| sources.iter().find(|s| s.metadata().name == name).unwrap();
    let head = find("head").execute(Some(path)).unwrap().fields;
    assert!(!head.contains_key("branch"), "{head:?}");
    assert_eq!(head.get("detached").unwrap().as_text(), "false");
    for name in ["refs", "status"] {
        let err = find(name).execute(Some(path)).unwrap_err();
        assert_eq!(
            err,
            format!("git.{name}: data: plugin(): {name} in git: returned NULL")
        );
    }
}

/// A gitfile that does not say `gitdir:` is no repository: nothing to report,
/// which is not a failure.
#[test]
fn git_a_gitfile_that_names_nothing_is_no_repository() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join(".git"), "not a gitfile\n").unwrap();
    let path = tmp.path().to_str().unwrap();
    for source in git_sources() {
        let got = source.execute(Some(path)).unwrap();
        assert!(
            got.fields.is_empty(),
            "{}: {:?}",
            source.metadata().name,
            got.fields
        );
    }
}

/// A submodule's gitfile is relative, and its gitdir has no `commondir`.
#[test]
fn git_a_relative_gitfile_is_read_against_the_root() {
    let repo = GitRepoFixture::new();
    let sub = TempDir::new().unwrap();
    let gitdir = repo.path().canonicalize().unwrap().join(".git");
    let relative = relative_to(&sub.path().canonicalize().unwrap(), &gitdir);
    std::fs::write(
        sub.path().join(".git"),
        format!("gitdir: {}\n", relative.display()),
    )
    .unwrap();
    let sources = git_sources();
    let head = sources
        .iter()
        .find(|s| s.metadata().name == "head")
        .unwrap();
    let got = head.execute(Some(sub.path().to_str().unwrap())).unwrap();
    assert_eq!(got.fields.get("branch").unwrap().as_text(), "main");
}

/// `to`, relative to the directory `from`.
fn relative_to(from: &Path, to: &Path) -> std::path::PathBuf {
    let shared = from
        .components()
        .zip(to.components())
        .take_while(|(a, b)| a == b)
        .count();
    let ups = from.components().count() - shared;
    let mut out = std::path::PathBuf::new();
    out.extend(std::iter::repeat_n("..", ups));
    out.extend(to.components().skip(shared));
    out
}

// ── what a read checks ────────────────────────────────────────────────────────

/// Point `gitdir`'s HEAD at `branch` as git does: a lockfile renamed over it.
fn write_head(gitdir: &Path, branch: &str) {
    let lock = gitdir.join("HEAD.lock");
    std::fs::write(&lock, format!("ref: refs/heads/{branch}\n")).unwrap();
    std::fs::rename(&lock, gitdir.join("HEAD")).unwrap();
}

/// With the watcher off, at `dir`: 100 checkouts in a row are each seen by the
/// next read of the branch; a commit by the next read of the commit, and a
/// `git add` by the next read of the staged count, the plugin's reads being
/// checked by what the Source's own lines stat.
fn every_change_is_seen(repo: &GitRepoFixture, dir: &Path, gitdir: &Path) {
    let s = served(&[], "");
    let at = Some(dir);
    let branch = s.get("git.branch", at).expect("a branch");
    for i in 0..100 {
        let name = format!("b{i}");
        write_head(gitdir, &name);
        assert_eq!(s.get("git.branch", at), Some(json!(name)), "HEAD write {i}");
    }
    write_head(gitdir, branch.as_str().unwrap());
    assert_eq!(s.get("git.branch", at), Some(branch));

    let before = s.get("git.commit", at).expect("a commit");
    std::fs::write(dir.join("c.txt"), "c").unwrap();
    assert_eq!(s.get("git.staged", at), Some(json!(0)));
    repo.git_in(dir, &["add", "c.txt"]);
    assert_eq!(s.get("git.staged", at), Some(json!(1)), "git add");
    repo.git_in(dir, &["commit", "-qm", "one more"]);
    let after = s.get("git.commit", at).expect("a commit");
    assert_ne!(before, after, "git commit");
    assert_eq!(s.get("git.commit_summary", at), Some(json!("one more")));
    assert_eq!(
        s.get("git.staged", at),
        Some(json!(0)),
        "the commit's index"
    );

    // Refs packed: the loose ref goes, and packed-refs says where it went.
    repo.git_in(dir, &["pack-refs", "--all"]);
    std::fs::write(dir.join("d.txt"), "d").unwrap();
    repo.git_in(dir, &["add", "d.txt"]);
    repo.git_in(dir, &["commit", "-qm", "after packing"]);
    assert_eq!(
        s.get("git.commit_summary", at),
        Some(json!("after packing"))
    );
}

#[test]
fn git_inputs_are_the_files_git_reads() {
    let repo = GitRepoFixture::new();
    let dir = repo.path().canonicalize().unwrap();
    every_change_is_seen(&repo, &dir, &dir.join(".git"));
}

#[test]
fn git_inputs_are_the_files_git_reads_in_a_linked_worktree() {
    let repo = GitRepoFixture::new();
    let wt = repo.add_worktree("wt", "start").canonicalize().unwrap();
    let gitdir = repo
        .path()
        .canonicalize()
        .unwrap()
        .join(".git/worktrees/wt");
    every_change_is_seen(&repo, &wt, &gitdir);
}

/// Reads that find nothing changed run nothing: not one process per read.
#[test]
fn git_reads_with_nothing_changed_run_nothing() {
    let repo = GitRepoFixture::new();
    let dir = repo.path().canonicalize().unwrap();
    for path in [".git", ".git/HEAD", ".git/index", ".git/refs/heads/main"] {
        age(&dir.join(path));
    }
    let s = served(&[], "");
    s.get("git", Some(&dir)).expect("git answers");
    let ran = |source: &str| s.ran_at("git", Some(&dir), source);
    let every = || -> Vec<_> {
        ["head", "refs", "diff", "lineage", "status"]
            .into_iter()
            .map(ran)
            .collect()
    };
    let first = every();
    for _ in 0..50 {
        s.get("git", Some(&dir)).expect("git answers");
    }
    assert_eq!(first, every());
}

// ── what it spawns ────────────────────────────────────────────────────────────

/// A `git` that logs each command and its environment, then runs the real one.
fn logging_git(dir: &Path, log: &Path) {
    let real = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|d| d.join("git"))
        .find(|p| p.is_file())
        .expect("git on PATH");
    let script = dir.join("git");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             echo \"$GIT_OPTIONAL_LOCKS $GIT_TERMINAL_PROMPT $GIT_ASKPASS $SSH_ASKPASS \
             $GCM_INTERACTIVE $*\" >> '{}'\n\
             exec '{}' \"$@\"\n",
            log.display(),
            real.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A refresh spawns the git commands the Rust provider did, in git's
/// environment, and nothing else: `head` none, no shell, no helper.
#[test]
fn git_a_refresh_spawns_the_same_commands() {
    let repo = GitRepoFixture::new();
    let tools = TempDir::new().unwrap();
    let log = tools.path().join("log");
    logging_git(tools.path(), &log);
    let path = std::env::join_paths(
        std::iter::once(tools.path().to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    // SAFETY: nextest gives each test its own process, and nothing else reads
    // the environment until the server below starts.
    unsafe { std::env::set_var("PATH", path) };
    let s = served(&[], "");
    s.get("git", Some(repo.path())).expect("git answers");
    let text = std::fs::read_to_string(&log).expect("git ran");
    let mut commands: Vec<&str> = text.lines().collect();
    commands.sort_unstable();
    let guards = "0 0 true true Never";
    let point = crate::common::git::git_cmd(repo.path(), &["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let point = String::from_utf8_lossy(&point.stdout);
    let mut want: Vec<String> = [
        "status --porcelain=v2 --branch", // refs
        "log -1 --format=%h %ct %s",
        "describe --tags --abbrev=0",
        "config --get branch.main.pushRemote",
        "config --get remote.pushDefault",
        "diff --numstat", // diff
        "diff --cached --numstat",
        "rev-parse --verify --quiet refs/heads/main", // lineage: the default branch
        "merge-base --fork-point main HEAD",
        &format!("diff --numstat {}..HEAD", point.trim()),
        "status --porcelain=v2 --branch", // status
    ]
    .iter()
    .map(|c| format!("{guards} {c}"))
    .collect();
    want.sort_unstable();
    assert_eq!(commands, want);
}

/// The Linux self-trigger class (`4a636cc`): a Source that runs git writes
/// nothing its watch sees, so an idle daemon does not run it again. Without
/// `GIT_OPTIONAL_LOCKS=0`, `git status` rewrites the index it watches.
#[cfg(target_os = "linux")]
#[test]
fn git_status_does_not_trigger_itself() {
    let repo = GitRepoFixture::new();
    // A stale index, which a `git status` without the guard would rewrite.
    let index = repo.path().join(".git/index");
    age(&index);
    std::fs::write(repo.path().join("README.md"), "# TEST").unwrap();
    let tools = TempDir::new().unwrap();
    let log = tools.path().join("log");
    logging_git(tools.path(), &log);
    let socket = crate::common::socket::IsolatedSocket::new();
    let home = socket.dir.path().join("home");
    let xdg = socket.dir.path().join("xdg");
    let daemon =
        crate::common::daemon::TestDaemon::spawn_with_tools(socket, home, xdg, Some(tools.path()));
    let statuses = || {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains(" status "))
            .count()
    };
    daemon
        .comb(repo.path())
        .args(["get", "-f", "json", "git.dirty"])
        .assert()
        .success();
    let first = statuses();
    assert!(first >= 1, "the read ran status");
    let modified = std::fs::metadata(&index).unwrap().modified().unwrap();
    // T7.10: the assertion is that nothing happens for 2 s of idle.
    // sleep-justified: there is no condition to wait on for nothing happening.
    std::thread::sleep(Duration::from_secs(2));
    assert!(
        statuses() <= first + 1,
        "{first} status runs became {}: it triggers itself",
        statuses()
    );
    assert_eq!(
        std::fs::metadata(&index).unwrap().modified().unwrap(),
        modified,
        "no status wrote the index"
    );
}
