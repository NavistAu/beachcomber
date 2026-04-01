use crate::provider::{
    FieldSchema, FieldType, InvalidationStrategy, Provider, ProviderMetadata, ProviderResult, Value,
};
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct GitProvider;

impl Provider for GitProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "git".to_string(),
            fields: vec![
                FieldSchema {
                    name: "branch".to_string(),
                    field_type: FieldType::String,
                },
                FieldSchema {
                    name: "dirty".to_string(),
                    field_type: FieldType::Bool,
                },
                FieldSchema {
                    name: "staged".to_string(),
                    field_type: FieldType::Int,
                },
                FieldSchema {
                    name: "unstaged".to_string(),
                    field_type: FieldType::Int,
                },
                FieldSchema {
                    name: "untracked".to_string(),
                    field_type: FieldType::Int,
                },
                FieldSchema {
                    name: "conflicted".to_string(),
                    field_type: FieldType::Int,
                },
                FieldSchema {
                    name: "ahead".to_string(),
                    field_type: FieldType::Int,
                },
                FieldSchema {
                    name: "behind".to_string(),
                    field_type: FieldType::Int,
                },
                FieldSchema {
                    name: "stash".to_string(),
                    field_type: FieldType::Int,
                },
                FieldSchema {
                    name: "state".to_string(),
                    field_type: FieldType::String,
                },
                FieldSchema {
                    name: "lines_added".to_string(),
                    field_type: FieldType::Int,
                },
                FieldSchema {
                    name: "lines_removed".to_string(),
                    field_type: FieldType::Int,
                },
                FieldSchema {
                    name: "lines_staged_added".to_string(),
                    field_type: FieldType::Int,
                },
                FieldSchema {
                    name: "lines_staged_removed".to_string(),
                    field_type: FieldType::Int,
                },
                FieldSchema {
                    name: "upstream".to_string(),
                    field_type: FieldType::String,
                },
                FieldSchema {
                    name: "detached".to_string(),
                    field_type: FieldType::Bool,
                },
                FieldSchema {
                    name: "commit".to_string(),
                    field_type: FieldType::String,
                },
                FieldSchema {
                    name: "tag".to_string(),
                    field_type: FieldType::String,
                },
                FieldSchema {
                    name: "state_step".to_string(),
                    field_type: FieldType::Int,
                },
                FieldSchema {
                    name: "state_total".to_string(),
                    field_type: FieldType::Int,
                },
                FieldSchema {
                    name: "last_commit_age_secs".to_string(),
                    field_type: FieldType::Int,
                },
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
        let (state, state_step, state_total) = detect_repo_state(dir);

        let dirty = status.staged > 0
            || status.unstaged > 0
            || status.untracked > 0
            || status.conflicted > 0;

        let (lines_added, lines_removed) = diff_numstat(dir);
        let (lines_staged_added, lines_staged_removed) = diff_numstat_staged(dir);
        let (commit, last_commit_ts) = get_head_info(dir);
        let tag = get_nearest_tag(dir);

        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let last_commit_age_secs = if last_commit_ts > 0 {
            now_secs.saturating_sub(last_commit_ts)
        } else {
            0
        };

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
        result.insert("lines_added", Value::Int(lines_added));
        result.insert("lines_removed", Value::Int(lines_removed));
        result.insert("lines_staged_added", Value::Int(lines_staged_added));
        result.insert("lines_staged_removed", Value::Int(lines_staged_removed));
        result.insert("upstream", Value::String(status.upstream));
        result.insert("detached", Value::Bool(status.detached));
        result.insert("commit", Value::String(commit));
        result.insert("tag", Value::String(tag));
        result.insert("state_step", Value::Int(state_step));
        result.insert("state_total", Value::Int(state_total));
        result.insert("last_commit_age_secs", Value::Int(last_commit_age_secs));
        Some(result)
    }
}

struct GitStatus {
    branch: String,
    upstream: String,
    detached: bool,
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

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut branch = String::new();
    let mut upstream = String::new();
    let mut detached = false;
    let mut ahead: i64 = 0;
    let mut behind: i64 = 0;
    let mut staged: i64 = 0;
    let mut unstaged: i64 = 0;
    let mut untracked: i64 = 0;
    let mut conflicted: i64 = 0;

    for line in stdout.lines() {
        if line.starts_with("# branch.head ") {
            let head = line.strip_prefix("# branch.head ").unwrap_or("");
            if head == "(detached)" {
                detached = true;
                branch = head.to_string();
            } else {
                branch = head.to_string();
            }
        } else if line.starts_with("# branch.upstream ") {
            upstream = line
                .strip_prefix("# branch.upstream ")
                .unwrap_or("")
                .to_string();
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

    Some(GitStatus {
        branch,
        upstream,
        detached,
        ahead,
        behind,
        staged,
        unstaged,
        untracked,
        conflicted,
    })
}

fn count_stashes(dir: &Path) -> i64 {
    let stash_log = dir.join(".git").join("logs").join("refs").join("stash");
    std::fs::read_to_string(&stash_log)
        .map(|s| s.lines().count() as i64)
        .unwrap_or(0)
}

/// Returns (state_name, step, total).
fn detect_repo_state(dir: &Path) -> (String, i64, i64) {
    let git_dir = dir.join(".git");

    if git_dir.join("MERGE_HEAD").exists() {
        return ("merge".to_string(), 0, 0);
    }

    if git_dir.join("rebase-merge").exists() {
        let step = read_int_file(&git_dir.join("rebase-merge").join("msgnum"));
        let total = read_int_file(&git_dir.join("rebase-merge").join("end"));
        return ("rebase".to_string(), step, total);
    }

    if git_dir.join("rebase-apply").exists() {
        let step = read_int_file(&git_dir.join("rebase-apply").join("next"));
        let total = read_int_file(&git_dir.join("rebase-apply").join("last"));
        return ("rebase".to_string(), step, total);
    }

    if git_dir.join("CHERRY_PICK_HEAD").exists() {
        return ("cherry-pick".to_string(), 0, 0);
    }

    if git_dir.join("BISECT_LOG").exists() {
        return ("bisect".to_string(), 0, 0);
    }

    if git_dir.join("REVERT_HEAD").exists() {
        return ("revert".to_string(), 0, 0);
    }

    ("clean".to_string(), 0, 0)
}

fn read_int_file(path: &Path) -> i64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Runs `git diff --numstat` and returns (lines_added, lines_removed) summed across all files.
fn diff_numstat(dir: &Path) -> (i64, i64) {
    let output = Command::new("git")
        .args(["diff", "--numstat"])
        .current_dir(dir)
        .output();

    parse_numstat_output(output)
}

/// Runs `git diff --cached --numstat` and returns (lines_added, lines_removed) summed.
fn diff_numstat_staged(dir: &Path) -> (i64, i64) {
    let output = Command::new("git")
        .args(["diff", "--cached", "--numstat"])
        .current_dir(dir)
        .output();

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

/// Runs `git log -1 --format="%h %ct"` and returns (short_hash, commit_timestamp).
fn get_head_info(dir: &Path) -> (String, i64) {
    let output = Command::new("git")
        .args(["log", "-1", "--format=%h %ct"])
        .current_dir(dir)
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return (String::new(), 0),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.trim();
    let mut parts = line.splitn(2, ' ');
    let hash = parts.next().unwrap_or("").to_string();
    let ts: i64 = parts
        .next()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    (hash, ts)
}

/// Runs `git describe --tags --abbrev=0` and returns the nearest tag or empty string.
fn get_nearest_tag(dir: &Path) -> String {
    let output = Command::new("git")
        .args(["describe", "--tags", "--abbrev=0"])
        .current_dir(dir)
        .output();

    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => String::new(),
    }
}

fn is_inside_git_repo(dir: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
