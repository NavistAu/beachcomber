use beachcomber::provider::FieldScope;
use beachcomber::provider::InvalidationStrategy;
use beachcomber::provider::Provider;
use beachcomber::provider::git::GitProvider;
use std::process::Command;
use tempfile::TempDir;

fn create_test_repo() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::fs::write(dir.join("README.md"), "# test").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(dir)
        .output()
        .unwrap();
    tmp
}

#[test]
fn git_provider_metadata() {
    let p = GitProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "git");
    assert_eq!(
        meta.inferred_scope(),
        FieldScope::PathScoped,
        "git should be path-scoped"
    );
    let field_names: Vec<&str> = meta.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(field_names.contains(&"branch"));
    assert!(field_names.contains(&"dirty"));
    assert!(field_names.contains(&"ahead"));
    assert!(field_names.contains(&"behind"));
    assert!(field_names.contains(&"staged"));
    assert!(field_names.contains(&"unstaged"));
    assert!(field_names.contains(&"untracked"));
    assert!(field_names.contains(&"conflicted"));
    assert!(field_names.contains(&"stash"));
    assert!(field_names.contains(&"state"));
    assert!(field_names.contains(&"lines_added"));
    assert!(field_names.contains(&"lines_removed"));
    assert!(field_names.contains(&"lines_staged_added"));
    assert!(field_names.contains(&"lines_staged_removed"));
    assert!(field_names.contains(&"upstream"));
    assert!(field_names.contains(&"detached"));
    assert!(field_names.contains(&"commit"));
    assert!(field_names.contains(&"tag"));
    assert!(field_names.contains(&"state_step"));
    assert!(field_names.contains(&"state_total"));
    assert!(field_names.contains(&"last_commit_age_secs"));
    assert!(field_names.contains(&"commit_summary"));
    assert!(field_names.contains(&"push_ahead"));
    assert!(field_names.contains(&"push_behind"));
}

#[test]
fn git_provider_invalidation_is_watch_and_poll() {
    let p = GitProvider;
    match p.metadata().invalidation {
        InvalidationStrategy::WatchAndPoll { ref patterns, .. } => {
            assert!(
                patterns.iter().any(|p| p.contains(".git")),
                "Should watch .git directory"
            );
        }
        _ => panic!("Expected WatchAndPoll invalidation"),
    }
}

#[test]
fn git_provider_returns_none_for_non_repo() {
    let tmp = TempDir::new().unwrap();
    let p = GitProvider;
    let result = p.execute(Some(tmp.path().to_str().unwrap()));
    assert!(
        result.is_empty(),
        "Non-git directory should return empty Vec"
    );
}

#[test]
fn git_provider_returns_branch() {
    let tmp = create_test_repo();
    let p = GitProvider;
    let (_, result) = p
        .execute(Some(tmp.path().to_str().unwrap()))
        .into_iter()
        .next()
        .unwrap();
    let branch = result.get("branch").unwrap().as_text();
    assert!(!branch.is_empty(), "Branch should not be empty");
}

#[test]
fn git_provider_clean_repo() {
    let tmp = create_test_repo();
    let p = GitProvider;
    let (_, result) = p
        .execute(Some(tmp.path().to_str().unwrap()))
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(result.get("dirty").unwrap().as_text(), "false");
    assert_eq!(result.get("staged").unwrap().as_text(), "0");
    assert_eq!(result.get("unstaged").unwrap().as_text(), "0");
    assert_eq!(result.get("untracked").unwrap().as_text(), "0");
}

#[test]
fn git_provider_dirty_repo() {
    let tmp = create_test_repo();
    std::fs::write(tmp.path().join("new_file.txt"), "content").unwrap();
    let p = GitProvider;
    let (_, result) = p
        .execute(Some(tmp.path().to_str().unwrap()))
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(result.get("dirty").unwrap().as_text(), "true");
    assert_eq!(result.get("untracked").unwrap().as_text(), "1");
}

#[test]
fn git_provider_staged_changes() {
    let tmp = create_test_repo();
    std::fs::write(tmp.path().join("staged.txt"), "content").unwrap();
    Command::new("git")
        .args(["add", "staged.txt"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let p = GitProvider;
    let (_, result) = p
        .execute(Some(tmp.path().to_str().unwrap()))
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(result.get("staged").unwrap().as_text(), "1");
}

#[test]
fn git_provider_unstaged_changes() {
    let tmp = create_test_repo();
    std::fs::write(tmp.path().join("README.md"), "modified").unwrap();
    let p = GitProvider;
    let (_, result) = p
        .execute(Some(tmp.path().to_str().unwrap()))
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(result.get("unstaged").unwrap().as_text(), "1");
}

#[test]
fn git_provider_stash_count() {
    let tmp = create_test_repo();
    std::fs::write(tmp.path().join("README.md"), "stash me").unwrap();
    Command::new("git")
        .args(["stash"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let p = GitProvider;
    let (_, result) = p
        .execute(Some(tmp.path().to_str().unwrap()))
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(result.get("stash").unwrap().as_text(), "1");
}

#[test]
fn git_provider_requires_path() {
    let p = GitProvider;
    assert!(
        p.execute(None).is_empty(),
        "Git provider should return empty Vec without a path"
    );
}

#[test]
fn git_provider_clean_repo_new_fields() {
    let tmp = create_test_repo();
    let p = GitProvider;
    let (_, result) = p
        .execute(Some(tmp.path().to_str().unwrap()))
        .into_iter()
        .next()
        .unwrap();

    // No unstaged line changes in a clean repo
    assert_eq!(result.get("lines_added").unwrap().as_text(), "0");
    assert_eq!(result.get("lines_removed").unwrap().as_text(), "0");
    // No staged line changes in a clean repo
    assert_eq!(result.get("lines_staged_added").unwrap().as_text(), "0");
    assert_eq!(result.get("lines_staged_removed").unwrap().as_text(), "0");
    // No upstream in a local-only repo
    assert_eq!(result.get("upstream").unwrap().as_text(), "");
    // HEAD is not detached after a normal commit
    assert_eq!(result.get("detached").unwrap().as_text(), "false");
    // state_step and state_total are 0 in a clean repo
    assert_eq!(result.get("state_step").unwrap().as_text(), "0");
    assert_eq!(result.get("state_total").unwrap().as_text(), "0");
    // state is clean
    assert_eq!(result.get("state").unwrap().as_text(), "clean");
}

#[test]
fn git_provider_commit_hash_format() {
    let tmp = create_test_repo();
    let p = GitProvider;
    let (_, result) = p
        .execute(Some(tmp.path().to_str().unwrap()))
        .into_iter()
        .next()
        .unwrap();
    let commit = result.get("commit").unwrap().as_text();
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
fn git_provider_last_commit_age_secs() {
    let tmp = create_test_repo();
    let p = GitProvider;
    let (_, result) = p
        .execute(Some(tmp.path().to_str().unwrap()))
        .into_iter()
        .next()
        .unwrap();
    let age: i64 = result
        .get("last_commit_age_secs")
        .unwrap()
        .as_text()
        .parse()
        .unwrap();
    // The commit was just made, so age should be very small (< 60 seconds)
    assert!(age >= 0, "age should be non-negative");
    assert!(age < 60, "last_commit_age_secs should be recent: {age}");
}

#[test]
fn git_provider_lines_added_removed_unstaged() {
    let tmp = create_test_repo();
    // README.md has "# test" (1 line). Replace with 3 lines.
    std::fs::write(tmp.path().join("README.md"), "line1\nline2\nline3").unwrap();
    let p = GitProvider;
    let (_, result) = p
        .execute(Some(tmp.path().to_str().unwrap()))
        .into_iter()
        .next()
        .unwrap();
    let added: i64 = result
        .get("lines_added")
        .unwrap()
        .as_text()
        .parse()
        .unwrap();
    let removed: i64 = result
        .get("lines_removed")
        .unwrap()
        .as_text()
        .parse()
        .unwrap();
    // 3 lines added, 1 removed (the original "# test" line)
    assert!(added > 0, "lines_added should be > 0, got {added}");
    assert!(removed > 0, "lines_removed should be > 0, got {removed}");
}

#[test]
fn git_provider_lines_staged_added_removed() {
    let tmp = create_test_repo();
    // Stage an addition of a new file with 2 lines
    std::fs::write(tmp.path().join("new.txt"), "alpha\nbeta").unwrap();
    Command::new("git")
        .args(["add", "new.txt"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let p = GitProvider;
    let (_, result) = p
        .execute(Some(tmp.path().to_str().unwrap()))
        .into_iter()
        .next()
        .unwrap();
    let staged_added: i64 = result
        .get("lines_staged_added")
        .unwrap()
        .as_text()
        .parse()
        .unwrap();
    let staged_removed: i64 = result
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
fn git_provider_commit_summary() {
    let tmp = create_test_repo();
    let p = GitProvider;
    let (_, result) = p
        .execute(Some(tmp.path().to_str().unwrap()))
        .into_iter()
        .next()
        .unwrap();
    let summary = result.get("commit_summary").unwrap().as_text();
    assert_eq!(
        summary, "init",
        "commit_summary should be the first commit message"
    );
}

#[test]
fn git_provider_push_ahead_behind_no_push_remote() {
    let tmp = create_test_repo();
    let p = GitProvider;
    let (_, result) = p
        .execute(Some(tmp.path().to_str().unwrap()))
        .into_iter()
        .next()
        .unwrap();
    // No push remote configured — both should be 0
    assert_eq!(result.get("push_ahead").unwrap().as_text(), "0");
    assert_eq!(result.get("push_behind").unwrap().as_text(), "0");
}

#[test]
fn git_provider_detached_head() {
    let tmp = create_test_repo();
    // Get the commit hash so we can check it out detached
    let log_out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let sha = String::from_utf8_lossy(&log_out.stdout).trim().to_string();
    Command::new("git")
        .args(["checkout", "--detach", &sha])
        .current_dir(tmp.path())
        .output()
        .unwrap();

    let p = GitProvider;
    let (_, result) = p
        .execute(Some(tmp.path().to_str().unwrap()))
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(result.get("detached").unwrap().as_text(), "true");
}
