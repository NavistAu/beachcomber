use crate::provider::{
    FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};
use std::path::Path;
use std::process::Command;

pub struct GitProvider;

impl Provider for GitProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "git".to_string(),
            fields: vec![
                FieldSchema { name: "branch".to_string(), field_type: FieldType::String },
                FieldSchema { name: "dirty".to_string(), field_type: FieldType::Bool },
                FieldSchema { name: "staged".to_string(), field_type: FieldType::Int },
                FieldSchema { name: "unstaged".to_string(), field_type: FieldType::Int },
                FieldSchema { name: "untracked".to_string(), field_type: FieldType::Int },
                FieldSchema { name: "conflicted".to_string(), field_type: FieldType::Int },
                FieldSchema { name: "ahead".to_string(), field_type: FieldType::Int },
                FieldSchema { name: "behind".to_string(), field_type: FieldType::Int },
                FieldSchema { name: "stash".to_string(), field_type: FieldType::Int },
                FieldSchema { name: "state".to_string(), field_type: FieldType::String },
            ],
            invalidation: InvalidationStrategy::WatchAndPoll {
                patterns: vec![".git".to_string()],
                interval_secs: 60,
                floor_secs: 1,
            },
            global: false,
        }
    }

    fn execute(&self, path: Option<&str>) -> Option<ProviderResult> {
        let path = path?;
        let dir = Path::new(path);

        if !dir.join(".git").exists() && !is_inside_git_repo(dir) {
            return None;
        }

        let status = parse_git_status(dir)?;
        let stash_count = count_stashes(dir);
        let state = detect_repo_state(dir);

        let dirty = status.staged > 0 || status.unstaged > 0
            || status.untracked > 0 || status.conflicted > 0;

        let mut result = ProviderResult::new();
        result.insert("branch", Value::String(status.branch));
        result.insert("dirty", Value::Bool(dirty));
        result.insert("staged", Value::Int(status.staged));
        result.insert("unstaged", Value::Int(status.unstaged));
        result.insert("untracked", Value::Int(status.untracked));
        result.insert("conflicted", Value::Int(status.conflicted));
        result.insert("ahead", Value::Int(status.ahead));
        result.insert("behind", Value::Int(status.behind));
        result.insert("stash", Value::Int(stash_count));
        result.insert("state", Value::String(state));
        Some(result)
    }
}

struct GitStatus {
    branch: String,
    ahead: i64,
    behind: i64,
    staged: i64,
    unstaged: i64,
    untracked: i64,
    conflicted: i64,
}

fn parse_git_status(dir: &Path) -> Option<GitStatus> {
    let output = Command::new("git")
        .args(["status", "--porcelain=v2", "--branch"])
        .current_dir(dir)
        .output()
        .ok()?;

    if !output.status.success() { return None; }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut branch = String::new();
    let mut ahead: i64 = 0;
    let mut behind: i64 = 0;
    let mut staged: i64 = 0;
    let mut unstaged: i64 = 0;
    let mut untracked: i64 = 0;
    let mut conflicted: i64 = 0;

    for line in stdout.lines() {
        if line.starts_with("# branch.head ") {
            branch = line.strip_prefix("# branch.head ").unwrap_or("").to_string();
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
                if x != '.' { staged += 1; }
                if y != '.' { unstaged += 1; }
            }
        } else if line.starts_with("u ") {
            conflicted += 1;
        } else if line.starts_with("? ") {
            untracked += 1;
        }
    }

    Some(GitStatus { branch, ahead, behind, staged, unstaged, untracked, conflicted })
}

fn count_stashes(dir: &Path) -> i64 {
    let stash_log = dir.join(".git").join("logs").join("refs").join("stash");
    std::fs::read_to_string(&stash_log)
        .map(|s| s.lines().count() as i64)
        .unwrap_or(0)
}

fn detect_repo_state(dir: &Path) -> String {
    let git_dir = dir.join(".git");
    // Check in order of likelihood for early return
    if git_dir.join("MERGE_HEAD").exists() {
        return "merge".to_string();
    }
    if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
        return "rebase".to_string();
    }
    if git_dir.join("CHERRY_PICK_HEAD").exists() {
        return "cherry-pick".to_string();
    }
    if git_dir.join("BISECT_LOG").exists() {
        return "bisect".to_string();
    }
    if git_dir.join("REVERT_HEAD").exists() {
        return "revert".to_string();
    }
    "clean".to_string()
}

fn is_inside_git_repo(dir: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
