mod common;
use beachcomber::provider::Provider;
use beachcomber::provider::SourceScope;
use beachcomber::provider::git::GitProvider;
use common::git::GitRepoFixture;
use tempfile::TempDir;

#[test]
fn git_canonical_path_returns_repo_root_from_subdir() {
    let repo = GitRepoFixture::new();
    let subdir = repo.create_subdir("src/lib");

    let sources = GitProvider.sources();
    let got = sources[0].canonical_path(Some(subdir.to_str().unwrap()));
    let expected = repo.path().to_string_lossy().to_string();
    assert_eq!(got, Some(expected));
}

#[test]
fn git_canonical_path_returns_repo_root_when_called_at_root() {
    let repo = GitRepoFixture::new();
    let sources = GitProvider.sources();
    let got = sources[0].canonical_path(Some(repo.path_str()));
    let expected = repo.path().to_string_lossy().to_string();
    assert_eq!(got, Some(expected));
}

#[test]
fn git_canonical_path_returns_none_outside_any_repo() {
    let tmp = TempDir::new().unwrap();
    let sources = GitProvider.sources();
    let got = sources[0].canonical_path(Some(tmp.path().to_str().unwrap()));
    if let Some(got) = got {
        assert_ne!(
            got,
            tmp.path().to_string_lossy().to_string(),
            "tempdir has no .git; canonical_path should not return the dir itself"
        );
    }
}

#[test]
fn git_canonical_path_passes_none_through() {
    let sources = GitProvider.sources();
    assert_eq!(sources[0].canonical_path(None), None);
}

#[test]
fn git_provider_metadata() {
    let meta = GitProvider.metadata();
    assert_eq!(meta.name, "git");
    assert_eq!(meta.sources.len(), 5);

    let head_src = meta.sources.iter().find(|s| s.name == "head").unwrap();
    assert_eq!(head_src.scope, SourceScope::PathScoped);
    let head_fields: Vec<&str> = head_src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(head_fields.contains(&"branch"));
    assert!(head_fields.contains(&"detached"));

    let refs_src = meta.sources.iter().find(|s| s.name == "refs").unwrap();
    assert_eq!(refs_src.scope, SourceScope::PathScoped);
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

    let diff_src = meta.sources.iter().find(|s| s.name == "diff").unwrap();
    assert_eq!(diff_src.scope, SourceScope::PathScoped);
    let diff_fields: Vec<&str> = diff_src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(diff_fields.contains(&"lines_added"));
    assert!(diff_fields.contains(&"lines_removed"));
    assert!(diff_fields.contains(&"lines_staged_added"));
    assert!(diff_fields.contains(&"lines_staged_removed"));

    let lineage_src = meta.sources.iter().find(|s| s.name == "lineage").unwrap();
    assert_eq!(lineage_src.scope, SourceScope::PathScoped);
    let lineage_fields: Vec<&str> = lineage_src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(lineage_fields.contains(&"branch_parent"));
    assert!(lineage_fields.contains(&"lines_added_parent"));
    assert!(lineage_fields.contains(&"lines_removed_parent"));
    assert!(lineage_fields.contains(&"branch_default"));
    assert!(lineage_fields.contains(&"lines_added_default"));
    assert!(lineage_fields.contains(&"lines_removed_default"));

    let status_src = meta.sources.iter().find(|s| s.name == "status").unwrap();
    assert_eq!(status_src.scope, SourceScope::PathScoped);
    let status_fields: Vec<&str> = status_src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(status_fields.contains(&"staged"));
    assert!(status_fields.contains(&"unstaged"));
    assert!(status_fields.contains(&"untracked"));
    assert!(status_fields.contains(&"conflicted"));
    assert!(status_fields.contains(&"dirty"));
}

#[test]
fn git_provider_source_names() {
    let sources = GitProvider.sources();
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
    let sources = GitProvider.sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(tmp.path().to_str().unwrap()));
    assert!(
        result.fields.is_empty(),
        "Non-git directory should return empty SourceResult"
    );
}

#[test]
fn git_refs_returns_branch() {
    // branch and detached moved to the head source; verify head reports the branch.
    let repo = GitRepoFixture::new();
    let sources = GitProvider.sources();
    let head_src = sources
        .iter()
        .find(|s| s.metadata().name == "head")
        .unwrap();
    let result = head_src.execute(Some(repo.path_str()));
    let branch = result.fields.get("branch").unwrap().as_text();
    assert!(!branch.is_empty(), "Branch should not be empty");
}

#[test]
fn git_status_clean_repo() {
    let repo = GitRepoFixture::new();
    let sources = GitProvider.sources();
    let status_src = sources
        .iter()
        .find(|s| s.metadata().name == "status")
        .unwrap();
    let result = status_src.execute(Some(repo.path_str()));
    assert_eq!(result.fields.get("dirty").unwrap().as_text(), "false");
    assert_eq!(result.fields.get("staged").unwrap().as_text(), "0");
    assert_eq!(result.fields.get("unstaged").unwrap().as_text(), "0");
    assert_eq!(result.fields.get("untracked").unwrap().as_text(), "0");
}

#[test]
fn git_status_dirty_repo() {
    let repo = GitRepoFixture::new().with_untracked_file("new_file.txt", "content");
    let sources = GitProvider.sources();
    let status_src = sources
        .iter()
        .find(|s| s.metadata().name == "status")
        .unwrap();
    let result = status_src.execute(Some(repo.path_str()));
    assert_eq!(result.fields.get("dirty").unwrap().as_text(), "true");
    assert_eq!(result.fields.get("untracked").unwrap().as_text(), "1");
}

#[test]
fn git_status_staged_changes() {
    let repo = GitRepoFixture::new().with_staged_file("staged.txt", "content");
    let sources = GitProvider.sources();
    let status_src = sources
        .iter()
        .find(|s| s.metadata().name == "status")
        .unwrap();
    let result = status_src.execute(Some(repo.path_str()));
    assert_eq!(result.fields.get("staged").unwrap().as_text(), "1");
}

#[test]
fn git_status_unstaged_changes() {
    let repo = GitRepoFixture::new().with_unstaged_change("README.md", "modified");
    let sources = GitProvider.sources();
    let status_src = sources
        .iter()
        .find(|s| s.metadata().name == "status")
        .unwrap();
    let result = status_src.execute(Some(repo.path_str()));
    assert_eq!(result.fields.get("unstaged").unwrap().as_text(), "1");
}

#[test]
fn git_refs_stash_count() {
    let repo = GitRepoFixture::new()
        .with_unstaged_change("README.md", "stash me")
        .with_stash();
    let sources = GitProvider.sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(repo.path_str()));
    assert_eq!(result.fields.get("stash").unwrap().as_text(), "1");
}

#[test]
fn git_refs_requires_path() {
    let sources = GitProvider.sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(None);
    assert!(
        result.fields.is_empty(),
        "Git refs source should return empty SourceResult without a path"
    );
}

#[test]
fn git_refs_clean_repo_new_fields() {
    let repo = GitRepoFixture::new();
    let sources = GitProvider.sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(repo.path_str()));

    // No upstream in a local-only repo
    assert_eq!(result.fields.get("upstream").unwrap().as_text(), "");
    // state_step and state_total are 0 in a clean repo
    assert_eq!(result.fields.get("state_step").unwrap().as_text(), "0");
    assert_eq!(result.fields.get("state_total").unwrap().as_text(), "0");
    // state is clean
    assert_eq!(result.fields.get("state").unwrap().as_text(), "clean");

    // detached is now owned by the head source
    let head_src = sources
        .iter()
        .find(|s| s.metadata().name == "head")
        .unwrap();
    let head_result = head_src.execute(Some(repo.path_str()));
    // HEAD is not detached after a normal commit
    assert_eq!(
        head_result.fields.get("detached").unwrap().as_text(),
        "false"
    );
}

#[test]
fn git_refs_commit_hash_format() {
    let repo = GitRepoFixture::new();
    let sources = GitProvider.sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(repo.path_str()));
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
    let sources = GitProvider.sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(repo.path_str()));
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
    let sources = GitProvider.sources();
    let diff_src = sources
        .iter()
        .find(|s| s.metadata().name == "diff")
        .unwrap();
    let result = diff_src.execute(Some(repo.path_str()));
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
    let sources = GitProvider.sources();
    let diff_src = sources
        .iter()
        .find(|s| s.metadata().name == "diff")
        .unwrap();
    let result = diff_src.execute(Some(repo.path_str()));
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
    let sources = GitProvider.sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(repo.path_str()));
    let summary = result.fields.get("commit_summary").unwrap().as_text();
    assert_eq!(
        summary, "init",
        "commit_summary should be the first commit message"
    );
}

#[test]
fn git_refs_push_ahead_behind_no_push_remote() {
    let repo = GitRepoFixture::new();
    let sources = GitProvider.sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(repo.path_str()));
    // No push remote configured — both should be 0
    assert_eq!(result.fields.get("push_ahead").unwrap().as_text(), "0");
    assert_eq!(result.fields.get("push_behind").unwrap().as_text(), "0");
}

#[test]
fn git_refs_detached_head() {
    // detached is now owned by the head source.
    let repo = GitRepoFixture::new().with_detached_head();

    let sources = GitProvider.sources();
    let head_src = sources
        .iter()
        .find(|s| s.metadata().name == "head")
        .unwrap();
    let result = head_src.execute(Some(repo.path_str()));
    assert_eq!(result.fields.get("detached").unwrap().as_text(), "true");
}

#[test]
fn git_diff_clean_repo_has_zero_lines() {
    let repo = GitRepoFixture::new();
    let sources = GitProvider.sources();
    let diff_src = sources
        .iter()
        .find(|s| s.metadata().name == "diff")
        .unwrap();
    let result = diff_src.execute(Some(repo.path_str()));
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
    let meta = GitProvider.metadata();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for src in &meta.sources {
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
    if !common::git::has_git() {
        return;
    }
    let repo = GitRepoFixture::new();
    let wt = repo.add_worktree("wt", "feature");

    // Dirty the worktree's copy of README.md, then stash it from the worktree.
    std::fs::write(wt.join("README.md"), "stash me in the worktree").unwrap();
    repo.git_in(&wt, &["stash"]);

    let sources = GitProvider.sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(wt.to_str().unwrap()));

    // The stash reflog lives in the SHARED commondir, not <wt>/.git (a file).
    assert_eq!(
        result.fields.get("stash").unwrap().as_text(),
        "1",
        "stash created in a linked worktree must be counted via commondir"
    );
}

#[test]
fn git_refs_state_detected_in_linked_worktree() {
    if !common::git::has_git() {
        return;
    }
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

    let sources = GitProvider.sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(wt.to_str().unwrap()));

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
/// `branch: Created from <start>` rather than `HEAD`.
///
/// The initial branch is force-renamed to `main` because `git init`'s default
/// branch name depends on the host's `init.defaultBranch`.
fn lineage_fixture() -> GitRepoFixture {
    let repo = GitRepoFixture::new();
    repo.git_in(repo.path(), &["branch", "-M", "main"]);
    repo.with_branch_from("devel", "main")
        .with_staged_file("f.txt", "a\nb\nc\n")
        .with_commit("devel work")
        .with_branch_from("feature", "devel")
        .with_staged_file("g.txt", "x\ny\n")
        .with_commit("feature work")
}

fn lineage_fields(path: &str) -> std::collections::HashMap<String, String> {
    let sources = GitProvider.sources();
    let src = sources
        .iter()
        .find(|s| s.metadata().name == "lineage")
        .unwrap();
    src.execute(Some(path))
        .fields
        .iter()
        .map(|(k, v)| (k.clone(), v.as_text()))
        .collect()
}

#[test]
fn git_lineage_parent_from_reflog_and_counts_match_known_diff() {
    if !common::git::has_git() {
        return;
    }
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
    if !common::git::has_git() {
        return;
    }
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
    if !common::git::has_git() {
        return;
    }
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
    if !common::git::has_git() {
        return;
    }
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
    if !common::git::has_git() {
        return;
    }
    // `git checkout -b <new>` with no start point records `Created from HEAD`,
    // which names no branch.
    let repo = GitRepoFixture::new();
    repo.git_in(repo.path(), &["branch", "-M", "main"]);
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
    if !common::git::has_git() {
        return;
    }
    let repo = GitRepoFixture::new();
    repo.git_in(repo.path(), &["branch", "-M", "main"]);

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
    if !common::git::has_git() {
        return;
    }
    let repo = GitRepoFixture::new();
    repo.git_in(repo.path(), &["branch", "-M", "main"]);
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
    if !common::git::has_git() {
        return;
    }
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
fn git_lineage_requires_path() {
    let sources = GitProvider.sources();
    let src = sources
        .iter()
        .find(|s| s.metadata().name == "lineage")
        .unwrap();
    assert!(src.execute(None).fields.is_empty());
}

#[test]
fn git_lineage_parent_resolved_in_linked_worktree() {
    if !common::git::has_git() {
        return;
    }
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
    let sources = GitProvider.sources();
    let head_src = sources
        .iter()
        .find(|s| s.metadata().name == "head")
        .unwrap();

    let result = head_src.execute(Some(repo.path_str()));
    let branch = result.fields.get("branch").unwrap().as_text();
    assert!(!branch.is_empty(), "head source must report branch name");
    assert_eq!(
        result.fields.get("detached").unwrap().as_text(),
        "false",
        "head source must report detached=false on a normal checkout"
    );
}

#[test]
fn git_head_source_is_read_always() {
    let sources = GitProvider.sources();
    let head_src = sources
        .iter()
        .find(|s| s.metadata().name == "head")
        .unwrap();
    assert!(
        head_src.read_always(),
        "head source must return read_always() == true"
    );
}

#[test]
fn git_head_branch_correct_in_linked_worktree() {
    if !common::git::has_git() {
        return;
    }
    let repo = GitRepoFixture::new();
    // add_worktree creates a new branch "wt-branch" checked out in the linked worktree.
    let wt = repo.add_worktree("my-wt", "wt-branch");

    let sources = GitProvider.sources();
    let head_src = sources
        .iter()
        .find(|s| s.metadata().name == "head")
        .unwrap();

    let result = head_src.execute(Some(wt.to_str().unwrap()));
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
