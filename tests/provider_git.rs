use beachcomber::provider::Provider;
use beachcomber::provider::SourceScope;
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
fn git_canonical_path_returns_repo_root_from_subdir() {
    let tmp = create_test_repo();
    let repo = tmp.path();
    let subdir = repo.join("src").join("lib");
    std::fs::create_dir_all(&subdir).unwrap();

    let sources = GitProvider.sources();
    let got = sources[0].canonical_path(Some(subdir.to_str().unwrap()));
    let expected = repo.to_string_lossy().to_string();
    assert_eq!(got, Some(expected));
}

#[test]
fn git_canonical_path_returns_repo_root_when_called_at_root() {
    let tmp = create_test_repo();
    let repo = tmp.path();
    let sources = GitProvider.sources();
    let got = sources[0].canonical_path(Some(repo.to_str().unwrap()));
    let expected = repo.to_string_lossy().to_string();
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
    assert_eq!(meta.sources.len(), 3);

    let refs_src = meta.sources.iter().find(|s| s.name == "refs").unwrap();
    assert_eq!(refs_src.scope, SourceScope::PathScoped);
    let refs_fields: Vec<&str> = refs_src.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(refs_fields.contains(&"branch"));
    assert!(refs_fields.contains(&"ahead"));
    assert!(refs_fields.contains(&"behind"));
    assert!(refs_fields.contains(&"upstream"));
    assert!(refs_fields.contains(&"detached"));
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
    assert_eq!(sources.len(), 3);
    let names: Vec<&str> = sources.iter().map(|s| s.metadata().name.as_str()).collect();
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
    let tmp = create_test_repo();
    let sources = GitProvider.sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(tmp.path().to_str().unwrap()));
    let branch = result.fields.get("branch").unwrap().as_text();
    assert!(!branch.is_empty(), "Branch should not be empty");
}

#[test]
fn git_status_clean_repo() {
    let tmp = create_test_repo();
    let sources = GitProvider.sources();
    let status_src = sources
        .iter()
        .find(|s| s.metadata().name == "status")
        .unwrap();
    let result = status_src.execute(Some(tmp.path().to_str().unwrap()));
    assert_eq!(result.fields.get("dirty").unwrap().as_text(), "false");
    assert_eq!(result.fields.get("staged").unwrap().as_text(), "0");
    assert_eq!(result.fields.get("unstaged").unwrap().as_text(), "0");
    assert_eq!(result.fields.get("untracked").unwrap().as_text(), "0");
}

#[test]
fn git_status_dirty_repo() {
    let tmp = create_test_repo();
    std::fs::write(tmp.path().join("new_file.txt"), "content").unwrap();
    let sources = GitProvider.sources();
    let status_src = sources
        .iter()
        .find(|s| s.metadata().name == "status")
        .unwrap();
    let result = status_src.execute(Some(tmp.path().to_str().unwrap()));
    assert_eq!(result.fields.get("dirty").unwrap().as_text(), "true");
    assert_eq!(result.fields.get("untracked").unwrap().as_text(), "1");
}

#[test]
fn git_status_staged_changes() {
    let tmp = create_test_repo();
    std::fs::write(tmp.path().join("staged.txt"), "content").unwrap();
    Command::new("git")
        .args(["add", "staged.txt"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let sources = GitProvider.sources();
    let status_src = sources
        .iter()
        .find(|s| s.metadata().name == "status")
        .unwrap();
    let result = status_src.execute(Some(tmp.path().to_str().unwrap()));
    assert_eq!(result.fields.get("staged").unwrap().as_text(), "1");
}

#[test]
fn git_status_unstaged_changes() {
    let tmp = create_test_repo();
    std::fs::write(tmp.path().join("README.md"), "modified").unwrap();
    let sources = GitProvider.sources();
    let status_src = sources
        .iter()
        .find(|s| s.metadata().name == "status")
        .unwrap();
    let result = status_src.execute(Some(tmp.path().to_str().unwrap()));
    assert_eq!(result.fields.get("unstaged").unwrap().as_text(), "1");
}

#[test]
fn git_refs_stash_count() {
    let tmp = create_test_repo();
    std::fs::write(tmp.path().join("README.md"), "stash me").unwrap();
    Command::new("git")
        .args(["stash"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let sources = GitProvider.sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(tmp.path().to_str().unwrap()));
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
    let tmp = create_test_repo();
    let sources = GitProvider.sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(tmp.path().to_str().unwrap()));

    // No upstream in a local-only repo
    assert_eq!(result.fields.get("upstream").unwrap().as_text(), "");
    // HEAD is not detached after a normal commit
    assert_eq!(result.fields.get("detached").unwrap().as_text(), "false");
    // state_step and state_total are 0 in a clean repo
    assert_eq!(result.fields.get("state_step").unwrap().as_text(), "0");
    assert_eq!(result.fields.get("state_total").unwrap().as_text(), "0");
    // state is clean
    assert_eq!(result.fields.get("state").unwrap().as_text(), "clean");
}

#[test]
fn git_refs_commit_hash_format() {
    let tmp = create_test_repo();
    let sources = GitProvider.sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(tmp.path().to_str().unwrap()));
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
    let tmp = create_test_repo();
    let sources = GitProvider.sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(tmp.path().to_str().unwrap()));
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
    let tmp = create_test_repo();
    // README.md has "# test" (1 line). Replace with 3 lines.
    std::fs::write(tmp.path().join("README.md"), "line1\nline2\nline3").unwrap();
    let sources = GitProvider.sources();
    let diff_src = sources
        .iter()
        .find(|s| s.metadata().name == "diff")
        .unwrap();
    let result = diff_src.execute(Some(tmp.path().to_str().unwrap()));
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
    let tmp = create_test_repo();
    // Stage an addition of a new file with 2 lines
    std::fs::write(tmp.path().join("new.txt"), "alpha\nbeta").unwrap();
    Command::new("git")
        .args(["add", "new.txt"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let sources = GitProvider.sources();
    let diff_src = sources
        .iter()
        .find(|s| s.metadata().name == "diff")
        .unwrap();
    let result = diff_src.execute(Some(tmp.path().to_str().unwrap()));
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
    let tmp = create_test_repo();
    let sources = GitProvider.sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(tmp.path().to_str().unwrap()));
    let summary = result.fields.get("commit_summary").unwrap().as_text();
    assert_eq!(
        summary, "init",
        "commit_summary should be the first commit message"
    );
}

#[test]
fn git_refs_push_ahead_behind_no_push_remote() {
    let tmp = create_test_repo();
    let sources = GitProvider.sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(tmp.path().to_str().unwrap()));
    // No push remote configured — both should be 0
    assert_eq!(result.fields.get("push_ahead").unwrap().as_text(), "0");
    assert_eq!(result.fields.get("push_behind").unwrap().as_text(), "0");
}

#[test]
fn git_refs_detached_head() {
    let tmp = create_test_repo();
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

    let sources = GitProvider.sources();
    let refs_src = sources
        .iter()
        .find(|s| s.metadata().name == "refs")
        .unwrap();
    let result = refs_src.execute(Some(tmp.path().to_str().unwrap()));
    assert_eq!(result.fields.get("detached").unwrap().as_text(), "true");
}

#[test]
fn git_diff_clean_repo_has_zero_lines() {
    let tmp = create_test_repo();
    let sources = GitProvider.sources();
    let diff_src = sources
        .iter()
        .find(|s| s.metadata().name == "diff")
        .unwrap();
    let result = diff_src.execute(Some(tmp.path().to_str().unwrap()));
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
