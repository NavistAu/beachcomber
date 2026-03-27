use shellstate::provider::Provider;
use shellstate::provider::git::GitProvider;
use shellstate::provider::InvalidationStrategy;
use std::process::Command;
use tempfile::TempDir;

fn create_test_repo() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    Command::new("git").args(["init"]).current_dir(dir).output().unwrap();
    Command::new("git").args(["config", "user.email", "test@test.com"]).current_dir(dir).output().unwrap();
    Command::new("git").args(["config", "user.name", "Test"]).current_dir(dir).output().unwrap();
    std::fs::write(dir.join("README.md"), "# test").unwrap();
    Command::new("git").args(["add", "."]).current_dir(dir).output().unwrap();
    Command::new("git").args(["commit", "-m", "init"]).current_dir(dir).output().unwrap();
    tmp
}

#[test]
fn git_provider_metadata() {
    let p = GitProvider;
    let meta = p.metadata();
    assert_eq!(meta.name, "git");
    assert!(!meta.global, "git should be path-scoped");
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
}

#[test]
fn git_provider_invalidation_is_watch_and_poll() {
    let p = GitProvider;
    match p.metadata().invalidation {
        InvalidationStrategy::WatchAndPoll { ref patterns, .. } => {
            assert!(patterns.iter().any(|p| p.contains(".git")), "Should watch .git directory");
        }
        _ => panic!("Expected WatchAndPoll invalidation"),
    }
}

#[test]
fn git_provider_returns_none_for_non_repo() {
    let tmp = TempDir::new().unwrap();
    let p = GitProvider;
    let result = p.execute(Some(tmp.path().to_str().unwrap()));
    assert!(result.is_none(), "Non-git directory should return None");
}

#[test]
fn git_provider_returns_branch() {
    let tmp = create_test_repo();
    let p = GitProvider;
    let result = p.execute(Some(tmp.path().to_str().unwrap())).unwrap();
    let branch = result.get("branch").unwrap().as_text();
    assert!(!branch.is_empty(), "Branch should not be empty");
}

#[test]
fn git_provider_clean_repo() {
    let tmp = create_test_repo();
    let p = GitProvider;
    let result = p.execute(Some(tmp.path().to_str().unwrap())).unwrap();
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
    let result = p.execute(Some(tmp.path().to_str().unwrap())).unwrap();
    assert_eq!(result.get("dirty").unwrap().as_text(), "true");
    assert_eq!(result.get("untracked").unwrap().as_text(), "1");
}

#[test]
fn git_provider_staged_changes() {
    let tmp = create_test_repo();
    std::fs::write(tmp.path().join("staged.txt"), "content").unwrap();
    Command::new("git").args(["add", "staged.txt"]).current_dir(tmp.path()).output().unwrap();
    let p = GitProvider;
    let result = p.execute(Some(tmp.path().to_str().unwrap())).unwrap();
    assert_eq!(result.get("staged").unwrap().as_text(), "1");
}

#[test]
fn git_provider_unstaged_changes() {
    let tmp = create_test_repo();
    std::fs::write(tmp.path().join("README.md"), "modified").unwrap();
    let p = GitProvider;
    let result = p.execute(Some(tmp.path().to_str().unwrap())).unwrap();
    assert_eq!(result.get("unstaged").unwrap().as_text(), "1");
}

#[test]
fn git_provider_stash_count() {
    let tmp = create_test_repo();
    std::fs::write(tmp.path().join("README.md"), "stash me").unwrap();
    Command::new("git").args(["stash"]).current_dir(tmp.path()).output().unwrap();
    let p = GitProvider;
    let result = p.execute(Some(tmp.path().to_str().unwrap())).unwrap();
    assert_eq!(result.get("stash").unwrap().as_text(), "1");
}

#[test]
fn git_provider_requires_path() {
    let p = GitProvider;
    assert!(p.execute(None).is_none(), "Git provider should return None without a path");
}
