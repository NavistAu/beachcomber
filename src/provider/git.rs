use crate::provider::{
    FieldSchema, FieldScope, FieldType, InvalidationStrategy, Provider, ProviderMetadata,
    ProviderResult, Value,
};
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Builds a `git` command with defensive env vars for daemon-safe invocation:
/// - `GIT_OPTIONAL_LOCKS=0`: prevents `.git/index.lock` contention with concurrent user git ops.
/// - `GIT_TERMINAL_PROMPT=0`: never prompt on tty for credentials (would hang the daemon).
/// - `GIT_ASKPASS=true` / `SSH_ASKPASS=true`: suppress GUI credential prompts; git/ssh treat
///   the empty output from `true(1)` as "no credential available" and fail gracefully.
/// - `GCM_INTERACTIVE=Never`: disables interactive flows in Git Credential Manager.
fn git_cmd(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "true")
        .env("SSH_ASKPASS", "true")
        .env("GCM_INTERACTIVE", "Never")
        .current_dir(dir);
    cmd
}

pub struct GitProvider;

impl Provider for GitProvider {
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            name: "git".to_string(),
            fields: vec![
                FieldSchema {
                    name: "branch".to_string(),
                    field_type: FieldType::String,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "dirty".to_string(),
                    field_type: FieldType::Bool,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "staged".to_string(),
                    field_type: FieldType::Int,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "unstaged".to_string(),
                    field_type: FieldType::Int,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "untracked".to_string(),
                    field_type: FieldType::Int,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "conflicted".to_string(),
                    field_type: FieldType::Int,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "ahead".to_string(),
                    field_type: FieldType::Int,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "behind".to_string(),
                    field_type: FieldType::Int,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "stash".to_string(),
                    field_type: FieldType::Int,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "state".to_string(),
                    field_type: FieldType::String,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "lines_added".to_string(),
                    field_type: FieldType::Int,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "lines_removed".to_string(),
                    field_type: FieldType::Int,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "lines_staged_added".to_string(),
                    field_type: FieldType::Int,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "lines_staged_removed".to_string(),
                    field_type: FieldType::Int,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "upstream".to_string(),
                    field_type: FieldType::String,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "detached".to_string(),
                    field_type: FieldType::Bool,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "commit".to_string(),
                    field_type: FieldType::String,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "tag".to_string(),
                    field_type: FieldType::String,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "state_step".to_string(),
                    field_type: FieldType::Int,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "state_total".to_string(),
                    field_type: FieldType::Int,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "last_commit_age_secs".to_string(),
                    field_type: FieldType::Int,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "commit_summary".to_string(),
                    field_type: FieldType::String,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "push_ahead".to_string(),
                    field_type: FieldType::Int,
                    scope: FieldScope::PathScoped,
                },
                FieldSchema {
                    name: "push_behind".to_string(),
                    field_type: FieldType::Int,
                    scope: FieldScope::PathScoped,
                },
            ],
            invalidation: InvalidationStrategy::WatchAndPoll {
                patterns: vec![".git".to_string()],
                interval_secs: 60,
                floor_secs: 1,
            },
        }
    }

    fn execute(&self, path: Option<&str>) -> Vec<(Option<String>, ProviderResult)> {
        let Some(path) = path else {
            return Vec::new();
        };
        let path_owned = path.to_string();
        let dir = Path::new(path);

        if !dir.join(".git").exists() && !is_inside_git_repo(dir) {
            return Vec::new();
        }

        let Some(status) = parse_git_status(dir) else {
            return Vec::new();
        };
        let stash_count = count_stashes(dir);
        let (state, state_step, state_total) = detect_repo_state(dir);

        let dirty = status.staged > 0
            || status.unstaged > 0
            || status.untracked > 0
            || status.conflicted > 0;

        let (lines_added, lines_removed) = diff_numstat(dir);
        let (lines_staged_added, lines_staged_removed) = diff_numstat_staged(dir);
        let (commit, last_commit_ts, commit_summary) = get_head_info(dir);
        let tag = get_nearest_tag(dir);
        let (push_ahead, push_behind) = get_push_divergence(dir, &status.branch);

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
        result.insert("commit_summary", Value::String(commit_summary));
        result.insert("push_ahead", Value::Int(push_ahead));
        result.insert("push_behind", Value::Int(push_behind));
        vec![(Some(path_owned), result)]
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
    let output = git_cmd(dir)
        .args(["status", "--porcelain=v2", "--branch"])
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
    let output = git_cmd(dir).args(["diff", "--numstat"]).output();

    parse_numstat_output(output)
}

/// Runs `git diff --cached --numstat` and returns (lines_added, lines_removed) summed.
fn diff_numstat_staged(dir: &Path) -> (i64, i64) {
    let output = git_cmd(dir)
        .args(["diff", "--cached", "--numstat"])
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

/// Runs `git log -1 --format="%h %ct %s"` and returns (short_hash, commit_timestamp, subject).
fn get_head_info(dir: &Path) -> (String, i64, String) {
    let output = git_cmd(dir)
        .args(["log", "-1", "--format=%h %ct %s"])
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return (String::new(), 0, String::new()),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.trim();
    let mut parts = line.splitn(3, ' ');
    let hash = parts.next().unwrap_or("").to_string();
    let ts: i64 = parts
        .next()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let subject = parts.next().unwrap_or("").to_string();
    (hash, ts, subject)
}

/// Runs `git describe --tags --abbrev=0` and returns the nearest tag or empty string.
fn get_nearest_tag(dir: &Path) -> String {
    let output = git_cmd(dir)
        .args(["describe", "--tags", "--abbrev=0"])
        .output();

    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => String::new(),
    }
}

/// Returns (push_ahead, push_behind) relative to the push remote.
/// If no push remote is configured, returns (0, 0).
fn get_push_divergence(dir: &Path, branch: &str) -> (i64, i64) {
    if branch.is_empty() || branch == "(detached)" {
        return (0, 0);
    }

    // Resolve push remote: branch.<name>.pushRemote, then remote.pushDefault
    let push_remote = get_git_config(dir, &format!("branch.{branch}.pushRemote"))
        .or_else(|| get_git_config(dir, "remote.pushDefault"));

    let push_remote = match push_remote {
        Some(r) => r,
        None => return (0, 0),
    };

    let refspec = format!("{push_remote}/{branch}");

    // Check the ref exists before rev-list
    let check = git_cmd(dir)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/remotes/{refspec}"),
        ])
        .output();
    match check {
        Ok(o) if o.status.success() => {}
        _ => return (0, 0),
    }

    let output = git_cmd(dir)
        .args([
            "rev-list",
            "--count",
            "--left-right",
            &format!("HEAD...{refspec}"),
        ])
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return (0, 0),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = stdout.trim().split('\t').collect();
    if parts.len() == 2 {
        let ahead = parts[0].parse().unwrap_or(0);
        let behind = parts[1].parse().unwrap_or(0);
        (ahead, behind)
    } else {
        (0, 0)
    }
}

fn get_git_config(dir: &Path, key: &str) -> Option<String> {
    let output = git_cmd(dir).args(["config", "--get", key]).output().ok()?;
    if output.status.success() {
        let val = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if val.is_empty() { None } else { Some(val) }
    } else {
        None
    }
}

fn is_inside_git_repo(dir: &Path) -> bool {
    git_cmd(dir)
        .args(["rev-parse", "--git-dir"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
