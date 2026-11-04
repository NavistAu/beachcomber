//! `defaults/git.toml`: five Sources over the compiled-in `git` plugin, `head`
//! reading two files itself. The Sources are run from the shipped file as the
//! daemon runs them, on `GitRepoFixture` repositories; what a read does with
//! them is read through a daemon below.

use super::{age, scenario, schedule, served, served_watching};
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
    assert_eq!(meta.len(), 6);

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

    let branches_src = meta.iter().find(|s| s.name == "branches").unwrap();
    let branches_fields: Vec<&str> = branches_src
        .fields
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(branches_fields, ["branch_default", "branch_parent"]);

    let lineage_src = meta.iter().find(|s| s.name == "lineage").unwrap();
    let lineage_fields: Vec<&str> = lineage_src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(lineage_fields.contains(&"lines_added_parent"));
    assert!(lineage_fields.contains(&"lines_removed_parent"));
    assert!(lineage_fields.contains(&"lines_added_default"));
    assert!(lineage_fields.contains(&"lines_removed_default"));

    let status_src = meta.iter().find(|s| s.name == "status").unwrap();
    let status_fields: Vec<&str> = status_src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(status_fields.contains(&"staged"));
    assert!(status_fields.contains(&"unstaged"));
    assert!(status_fields.contains(&"untracked"));
    assert!(status_fields.contains(&"conflicted"));
    assert!(status_fields.contains(&"dirty"));
    assert!(status_fields.contains(&"ahead"));
    assert!(status_fields.contains(&"behind"));
    assert!(status_fields.contains(&"upstream"));
}

#[test]
fn git_provider_source_names() {
    let sources = git_sources();
    assert_eq!(sources.len(), 6);
    let names: Vec<&str> = sources.iter().map(|s| s.metadata().name.as_str()).collect();
    assert!(names.contains(&"branches"));
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
    sources
        .iter()
        .filter(|s| ["branches", "lineage"].contains(&s.metadata().name.as_str()))
        .flat_map(|s| s.execute(Some(path)).unwrap().fields)
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

/// Each Source's schedule, and that what wakes one is what it says something
/// about: no watch of `.git` itself, which a lockfile would set off.
#[test]
fn git_schedules_watch_what_each_source_reads() {
    let watch = |patterns: &[&str]| InvalidationStrategy::Watch {
        patterns: patterns.iter().map(|p| p.to_string()).collect(),
        abs_paths: vec![],
    };
    for (source, invalidation, failback) in [
        ("head", watch(&[".git/HEAD"]), 60),
        (
            "refs",
            watch(&[
                ".git/HEAD",
                ".git/refs",
                ".git/packed-refs",
                ".git/config",
                ".git/logs/refs/stash",
                ".git/MERGE_HEAD",
                ".git/rebase-merge",
                ".git/rebase-apply",
                ".git/CHERRY_PICK_HEAD",
                ".git/BISECT_LOG",
                ".git/REVERT_HEAD",
            ]),
            60,
        ),
        (
            "diff",
            InvalidationStrategy::WatchAndPoll {
                patterns: vec![".".into(), "!.git".into()],
                abs_paths: vec![],
                interval_secs: 30,
                count: 4,
            },
            60,
        ),
        (
            "branches",
            watch(&[".git/HEAD", ".git/refs", ".git/packed-refs"]),
            60,
        ),
        (
            "lineage",
            watch(&[".git/HEAD", ".git/refs", ".git/packed-refs"]),
            60,
        ),
        (
            "status",
            InvalidationStrategy::WatchAndPoll {
                patterns: [
                    ".git/index",
                    ".git/HEAD",
                    ".git/refs",
                    ".git/packed-refs",
                    ".git/config",
                    ".",
                    "!.git",
                ]
                .map(String::from)
                .to_vec(),
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
/// checked by the files it notes.
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
        ["head", "refs", "diff", "branches", "lineage", "status"]
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

// ── the first read after an event ─────────────────────────────────────────────

/// A repository on `main`, level with an upstream, with a branch `side` at the
/// same commit; every file in its `.git` old enough that a stamp of it is not
/// racy, and every Source warm.
struct FirstRead {
    repo: GitRepoFixture,
    _bare: TempDir,
    dir: std::path::PathBuf,
    s: super::Served,
}

impl FirstRead {
    fn new() -> Self {
        let repo = GitRepoFixture::new();
        let bare = TempDir::new().unwrap();
        let dir = repo.path().canonicalize().unwrap();
        let origin = bare.path().to_str().unwrap();
        repo.git_in(&dir, &["init", "-q", "--bare", origin]);
        repo.git_in(&dir, &["remote", "add", "origin", origin]);
        repo.git_in(&dir, &["push", "-q", "-u", "origin", "main"]);
        repo.git_in(&dir, &["branch", "side"]);
        let s = served(&[], "");
        let this = Self {
            repo,
            _bare: bare,
            dir,
            s,
        };
        this.settle();
        this
    }

    /// What has happened so far, long ago: the next change is a change.
    fn settle(&self) {
        for entry in walk(&self.dir.join(".git")) {
            age(&entry);
        }
        self.s.get("git", Some(&self.dir)).expect("git answers");
    }

    fn git(&self, args: &[&str]) {
        self.repo.git_in(&self.dir, args);
    }

    /// What the next read answers for `field`.
    fn get(&self, field: &str) -> Option<serde_json::Value> {
        self.s.get(&format!("git.{field}"), Some(&self.dir))
    }

    /// The whole of the next read, checked field by field against `want`.
    fn expect(&self, want: &[(&str, serde_json::Value)]) {
        let got = self.s.get("git", Some(&self.dir)).expect("git answers");
        for (field, value) in want {
            assert_eq!(got.get(*field), Some(value), "{field} of {got}");
        }
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.dir.join(name), body).unwrap();
    }

    fn sha(&self, rev: &str) -> String {
        let out = crate::common::git::git_cmd(&self.dir, &["rev-parse", "--short", rev])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}

/// Every path below `dir`, and `dir`; no object, which nothing reads a stamp of.
fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![dir.to_path_buf()];
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten().filter(|e| e.file_name() != "objects") {
            out.extend(walk(&entry.path()));
        }
    }
    out
}

fn tracked_file(f: &FirstRead) -> String {
    let out = crate::common::git::git_cmd(&f.dir, &["ls-files"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().next().expect("a tracked file").to_string()
}

/// A repository served with the host's own file watcher, once a change in its
/// tree is known to come through; `None` where none is delivered (a sandbox).
fn first_read_watching() -> Option<FirstRead> {
    let repo = GitRepoFixture::new();
    let dir = repo.path().canonicalize().unwrap();
    let f = FirstRead {
        repo,
        _bare: TempDir::new().unwrap(),
        dir,
        s: served_watching(),
    };
    f.settle();
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < until {
        f.write("probe.txt", "");
        if f.get("untracked") == Some(json!(1)) {
            std::fs::remove_file(f.dir.join("probe.txt")).unwrap();
            while f.get("untracked") != Some(json!(0)) && std::time::Instant::now() < until {}
            f.s.hear_file_events();
            return Some(f);
        }
    }
    eprintln!("skipped: this host's file watcher delivers nothing");
    None
}

/// `change` makes `field` say `want` at the next read, at once. A machine badly
/// behind can spend the read's wait for a file event: that read is old, and
/// one of a few being right says it is the machine and not the read.
fn right_straight_after(f: &FirstRead, field: &str, want: serde_json::Value, change: impl Fn(u32)) {
    for attempt in 0..20 {
        change(attempt);
        if f.get(field) == Some(want.clone()) {
            return;
        }
    }
    panic!("{field} was never {want} straight after the change");
}

/// Nothing in `.git` moves for a save, and nothing stat's the working tree: the
/// read hears of it from the file watcher, before it answers.
#[test]
fn first_read_after_an_edit() {
    let Some(f) = first_read_watching() else {
        return;
    };
    let file = tracked_file(&f);
    let body = std::fs::read_to_string(f.dir.join(&file)).unwrap();
    right_straight_after(&f, "unstaged", json!(1), |_| {
        f.write(&file, &format!("{body}more\n"));
    });
    assert_eq!(f.get("dirty"), Some(json!(true)));
    assert_eq!(f.get("lines_added"), Some(json!(1)));
}

#[test]
fn first_read_after_making_an_untracked_file() {
    let Some(f) = first_read_watching() else {
        return;
    };
    right_straight_after(&f, "dirty", json!(true), |attempt| {
        f.write(&format!("new{attempt}.txt"), "n\n");
    });
    assert!(f.get("untracked").is_some_and(|n| n != json!(0)));
}

/// A change in the tree runs nothing by itself: a build's output that no read
/// asks about costs no `git`. The next read runs what it reads.
#[test]
fn a_change_in_the_tree_waits_for_a_read() {
    let Some(f) = first_read_watching() else {
        return;
    };
    let ran = |source: &str| f.s.ran_at("git", Some(&f.dir), source);
    let before = (ran("status"), ran("diff"));
    let refs = ran("refs");
    f.write("new.txt", "n\n");
    f.s.hear_file_events();
    assert_eq!(before, (ran("status"), ran("diff")));
    assert_eq!(f.get("untracked"), Some(json!(1)));
    assert_ne!(before.0, ran("status"));
    assert_eq!(refs, ran("refs"));
}

/// A lockfile comes and goes: every record stands, no Source runs.
#[test]
fn first_read_after_a_lockfile_runs_nothing() {
    let f = FirstRead::new();
    let ran = |source: &str| f.s.ran_at("git", Some(&f.dir), source);
    let sources = ["head", "refs", "diff", "branches", "lineage", "status"];
    let before: Vec<_> = sources.iter().map(|s| ran(s)).collect();
    let lock = f.dir.join(".git/some.lock");
    std::fs::write(&lock, "").unwrap();
    std::fs::remove_file(&lock).unwrap();
    f.expect(&[("branch", json!("main")), ("dirty", json!(false))]);
    let after: Vec<_> = sources.iter().map(|s| ran(s)).collect();
    assert_eq!(before, after);
}

/// A plain `git status` rewrites a stat-dirty index: what depends on the index
/// runs again and says what it said; what does not, stands.
#[test]
fn first_read_after_git_status_is_unchanged() {
    let f = FirstRead::new();
    let file = tracked_file(&f);
    let body = std::fs::read(f.dir.join(&file)).unwrap();
    std::fs::write(f.dir.join(&file), body).unwrap();
    f.settle();
    let ran = |source: &str| f.s.ran_at("git", Some(&f.dir), source);
    let before = (ran("head"), ran("refs"), ran("branches"), ran("lineage"));
    f.git(&["status"]);
    f.expect(&[("dirty", json!(false)), ("staged", json!(0))]);
    assert_eq!(
        before,
        (ran("head"), ran("refs"), ran("branches"), ran("lineage"))
    );
}

#[test]
fn first_read_after_git_add() {
    let f = FirstRead::new();
    f.write("new.txt", "one\n");
    f.settle();
    f.git(&["add", "new.txt"]);
    f.expect(&[
        ("staged", json!(1)),
        ("untracked", json!(0)),
        ("dirty", json!(true)),
        ("lines_staged_added", json!(1)),
    ]);
}

/// `git add` moves the index and no ref: `refs`, `branches` and `lineage` do not run.
#[test]
fn git_add_runs_only_what_the_index_moves() {
    let f = FirstRead::new();
    f.write("new.txt", "one\n");
    f.settle();
    let ran = |source: &str| f.s.ran_at("git", Some(&f.dir), source);
    let before = (ran("head"), ran("refs"), ran("branches"), ran("lineage"));
    f.git(&["add", "new.txt"]);
    assert_eq!(f.get("staged"), Some(json!(1)));
    f.s.get("git", Some(&f.dir)).expect("git answers");
    assert_eq!(
        before,
        (ran("head"), ran("refs"), ran("branches"), ran("lineage"))
    );
}

#[test]
fn first_read_after_git_commit() {
    let f = FirstRead::new();
    f.write("new.txt", "one\n");
    f.git(&["add", "new.txt"]);
    f.settle();
    f.git(&["commit", "-qm", "the next one"]);
    f.expect(&[
        ("commit", json!(f.sha("HEAD"))),
        ("commit_summary", json!("the next one")),
        ("ahead", json!(1)),
        ("behind", json!(0)),
        ("staged", json!(0)),
        ("dirty", json!(false)),
        ("lines_staged_added", json!(0)),
        ("lines_added_default", json!(0)),
    ]);
}

#[test]
fn first_read_after_git_checkout() {
    let f = FirstRead::new();
    f.git(&["checkout", "-q", "-b", "elsewhere", "side"]);
    f.write("there.txt", "x\n");
    f.git(&["add", "there.txt"]);
    f.git(&["commit", "-qm", "on elsewhere"]);
    f.git(&["checkout", "-q", "main"]);
    f.settle();
    f.git(&["checkout", "-q", "elsewhere"]);
    f.expect(&[
        ("branch", json!("elsewhere")),
        ("commit", json!(f.sha("HEAD"))),
        ("commit_summary", json!("on elsewhere")),
    ]);
    let whole = f.s.get("git", Some(&f.dir)).unwrap();
    for gone in ["upstream", "ahead", "behind"] {
        assert_eq!(whole.get(gone), None, "{gone}: elsewhere has no upstream");
    }
}

#[test]
fn first_read_after_git_stash() {
    let f = FirstRead::new();
    let file = tracked_file(&f);
    f.write(&file, "changed\n");
    f.settle();
    f.git(&["stash", "-q"]);
    f.expect(&[
        ("stash", json!(1)),
        ("dirty", json!(false)),
        ("unstaged", json!(0)),
        ("lines_added", json!(0)),
    ]);
}

#[test]
fn first_read_after_git_fetch_sees_the_upstream_move() {
    let f = FirstRead::new();
    let other = TempDir::new().unwrap();
    let origin = crate::common::git::git_cmd(&f.dir, &["remote", "get-url", "origin"])
        .output()
        .unwrap();
    let origin = String::from_utf8_lossy(&origin.stdout).trim().to_string();
    f.git(&[
        "clone",
        "-q",
        "-b",
        "main",
        &origin,
        other.path().to_str().unwrap(),
    ]);
    let clone = other.path();
    std::fs::write(clone.join("up.txt"), "up\n").unwrap();
    f.repo.git_in(clone, &["add", "up.txt"]);
    f.repo.git_in(clone, &["commit", "-qm", "upstream moved"]);
    f.repo.git_in(clone, &["push", "-q", "origin", "HEAD:main"]);
    f.settle();
    assert_eq!(f.get("behind"), Some(json!(0)));
    f.git(&["fetch", "-q", "origin"]);
    assert_eq!(f.get("behind"), Some(json!(1)));
}

#[test]
fn first_read_after_git_push_sees_ahead_fall() {
    let f = FirstRead::new();
    f.write("new.txt", "one\n");
    f.git(&["add", "new.txt"]);
    f.git(&["commit", "-qm", "to push"]);
    f.settle();
    assert_eq!(f.get("ahead"), Some(json!(1)));
    f.git(&["push", "-q", "origin", "main"]);
    assert_eq!(f.get("ahead"), Some(json!(0)));
}

#[test]
fn first_read_after_git_rebase() {
    let f = FirstRead::new();
    f.git(&["checkout", "-q", "side"]);
    f.write("base.txt", "b\n");
    f.git(&["add", "base.txt"]);
    f.git(&["commit", "-qm", "the base moved"]);
    f.git(&["checkout", "-q", "main"]);
    f.write("mine.txt", "m\n");
    f.git(&["add", "mine.txt"]);
    f.git(&["commit", "-qm", "mine"]);
    f.settle();
    f.git(&["rebase", "-q", "side"]);
    f.expect(&[
        ("branch", json!("main")),
        ("commit", json!(f.sha("HEAD"))),
        ("commit_summary", json!("mine")),
        ("ahead", json!(2)),
        ("state", json!("clean")),
    ]);
}

/// A rebase stopped on a conflict: the operation, its step and the conflict.
#[test]
fn first_read_after_a_rebase_stops() {
    let f = FirstRead::new();
    let file = tracked_file(&f);
    f.git(&["checkout", "-q", "side"]);
    f.write(&file, "theirs\n");
    f.git(&["commit", "-qam", "theirs"]);
    f.git(&["checkout", "-q", "main"]);
    f.write(&file, "ours\n");
    f.git(&["commit", "-qam", "ours"]);
    f.settle();
    let out = crate::common::git::git_cmd(&f.dir, &["rebase", "-q", "side"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "a conflict");
    f.expect(&[
        ("state", json!("rebase")),
        ("state_step", json!(1)),
        ("state_total", json!(1)),
        ("conflicted", json!(1)),
        ("detached", json!(true)),
    ]);
}

/// A linked worktree is checked by the same files, where `.git` is a gitfile:
/// its index, its HEAD, and the refs it shares.
#[test]
fn first_read_after_events_in_a_linked_worktree() {
    let f = FirstRead::new();
    let wt = f.repo.add_worktree("wt", "start").canonicalize().unwrap();
    let at = Some(wt.as_path());
    let read = |field: &str| f.s.get(&format!("git.{field}"), at);
    assert_eq!(read("staged"), Some(json!(0)));
    let lock = f.dir.join(".git/worktrees/wt/some.lock");
    std::fs::write(&lock, "").unwrap();
    std::fs::remove_file(&lock).unwrap();
    assert_eq!(read("staged"), Some(json!(0)));

    std::fs::write(wt.join("w.txt"), "w\n").unwrap();
    f.repo.git_in(&wt, &["add", "w.txt"]);
    assert_eq!(read("staged"), Some(json!(1)), "git add");
    f.repo.git_in(&wt, &["commit", "-qm", "in the worktree"]);
    assert_eq!(read("commit_summary"), Some(json!("in the worktree")));
    assert_eq!(read("staged"), Some(json!(0)), "git commit");
    std::fs::write(wt.join("w.txt"), "w2\n").unwrap();
    f.repo.git_in(&wt, &["stash", "-q"]);
    assert_eq!(read("stash"), Some(json!(1)), "git stash");
}

/// The reads the owner of a Source gets from `lup`: a directory found is there,
/// whatever comes and goes in it, and made or removed nearer is a change.
#[test]
fn a_source_over_lup_does_not_run_for_entries_coming_and_going() {
    let s = served(
        &[],
        "[daemon.providers.m.sources.s]\n\
         root = \"{{ lup('.marker') | dirname }}\"\n\
         data = \"{{ lup('.marker') }}\"\n\
         result.found = \"{{ data }}\"\n\
         return = \"{{ result | poll('10m') }}\"\n",
    );
    let marker = s.project.join(".marker");
    std::fs::create_dir(&marker).unwrap();
    age(&marker);
    let at = Some(s.project.as_path());
    let found = s.get("m.found", at).expect("found");
    let first = s.ran_at("m", at, "s");
    std::fs::write(marker.join("entry"), "").unwrap();
    assert_eq!(s.get("m.found", at), Some(found));
    assert_eq!(s.ran_at("m", at, "s"), first, "an entry in it is no change");
    std::fs::remove_file(marker.join("entry")).unwrap();
    std::fs::remove_dir(&marker).unwrap();
    assert_eq!(s.get("m.found", at), None, "gone is a change");
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

/// A refresh spawns these, in git's environment, and nothing else: `head` none,
/// one `git status` for everything it says, one `git config` for both of its
/// keys, no `rev-parse` for a branch with a loose ref, no shell, no helper.
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
        "log -1 --format=%h %ct %s", // refs
        "describe --tags --abbrev=0",
        "config --list --null",
        "diff --numstat", // diff
        "diff --cached --numstat",
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
