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
    assert_eq!(meta.sources.len(), 4);

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
    assert_eq!(sources.len(), 4);
    let names: Vec<&str> = sources.iter().map(|s| s.metadata().name.as_str()).collect();
    assert!(names.contains(&"head"));
    assert!(names.contains(&"refs"));
    assert!(names.contains(&"diff"));
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
