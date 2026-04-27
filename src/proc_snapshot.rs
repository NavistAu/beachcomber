/// Exec-snapshot: captures process spawn events for a given duration and
/// returns structured data suitable for both terminal formatting (via
/// `comb check procs`) and the Introspect protocol op.
#[derive(Debug)]
pub struct ProcSample {
    pub command: String,
    pub count: u64,
    pub category: Option<String>,
}

#[derive(Debug)]
pub struct ReplacementSuggestion {
    pub command_pattern: String,
    pub provider: String,
    pub field: String,
}

#[derive(Debug)]
pub struct ProcSnapshotResult {
    pub duration_secs: u64,
    pub total: u64,
    pub samples: Vec<ProcSample>,
    pub replacement_suggestions: Vec<ReplacementSuggestion>,
}

/// Categories: (display_name, process_names, covered_by_beachcomber)
const CATEGORIES: &[(&str, &[&str], bool)] = &[
    ("git", &["git", "git-remote-https"], true),
    ("kubectl", &["kubectl"], true),
    ("gcloud", &["gcloud", "bq", "gsutil"], true),
    ("aws", &["aws"], true),
    ("terraform", &["terraform"], true),
    ("mise", &["mise"], true),
    ("direnv", &["direnv"], true),
    ("python", &["python", "python3", "pip", "pip3"], false),
    ("node", &["node", "npm", "npx", "yarn", "pnpm"], false),
    ("ruby", &["ruby", "gem", "bundle", "bundler"], false),
    ("shell", &["bash", "zsh", "sh", "fish"], false),
];

/// Replacement suggestions for covered categories.
/// (process_name, provider, field)
const REPLACEMENT_HINTS: &[(&str, &str, &str)] = &[
    ("git", "git", "branch"),
    ("git", "git", "dirty"),
    ("kubectl", "kubectl", "context"),
    ("gcloud", "gcloud", "account"),
    ("aws", "aws", "profile"),
    ("terraform", "terraform", "workspace"),
    ("mise", "mise", "version"),
    ("direnv", "direnv", "loaded"),
];

/// Convert a populated counts map into a `ProcSnapshotResult`, or return an
/// error if the map is empty.  This is the pure decision core shared by all
/// platform `capture` implementations.
pub fn decide_snapshot(
    duration_secs: u64,
    counts: std::collections::HashMap<String, u64>,
    empty_msg: &str,
) -> Result<ProcSnapshotResult, String> {
    if counts.is_empty() {
        return Err(empty_msg.to_string());
    }
    Ok(build_result(duration_secs, counts))
}

fn build_result(
    duration_secs: u64,
    counts: std::collections::HashMap<String, u64>,
) -> ProcSnapshotResult {
    let total: u64 = counts.values().sum();

    let categorized_set: std::collections::HashSet<&str> = CATEGORIES
        .iter()
        .flat_map(|(_, procs, _)| procs.iter().copied())
        .collect();

    let mut samples: Vec<ProcSample> = Vec::new();

    // Categorized samples.
    for (cat_name, procs, _covered) in CATEGORIES {
        let count: u64 = procs.iter().map(|p| counts.get(*p).unwrap_or(&0)).sum();
        if count > 0 {
            samples.push(ProcSample {
                command: cat_name.to_string(),
                count,
                category: Some(cat_name.to_string()),
            });
        }
    }

    // Uncategorized, sorted descending by count.
    let mut uncategorized: Vec<(&String, &u64)> = counts
        .iter()
        .filter(|(k, _)| !categorized_set.contains(k.as_str()))
        .collect();
    uncategorized.sort_by(|a, b| b.1.cmp(a.1));
    for (name, count) in uncategorized.iter().take(10) {
        samples.push(ProcSample {
            command: name.to_string(),
            count: **count,
            category: None,
        });
    }

    // Replacement suggestions: covered categories that actually appeared.
    let mut replacement_suggestions: Vec<ReplacementSuggestion> = Vec::new();
    for (cat_name, procs, covered) in CATEGORIES {
        if !covered {
            continue;
        }
        let count: u64 = procs.iter().map(|p| counts.get(*p).unwrap_or(&0)).sum();
        if count == 0 {
            continue;
        }
        for (hint_cat, provider, field) in REPLACEMENT_HINTS {
            if hint_cat == cat_name {
                replacement_suggestions.push(ReplacementSuggestion {
                    command_pattern: cat_name.to_string(),
                    provider: provider.to_string(),
                    field: field.to_string(),
                });
            }
        }
    }

    ProcSnapshotResult {
        duration_secs,
        total,
        samples,
        replacement_suggestions,
    }
}

/// Parse the NDJSON output produced by `eslogger exec` into a process-name
/// count map.  Each line is a JSON object; the executable path is found at
/// either `/process/executable/path` or
/// `/event/exec/target/executable/path`.  Lines that don't parse as JSON, or
/// don't contain either pointer, are silently skipped.
#[cfg(target_os = "macos")]
pub fn parse_eslogger_output(input: &str) -> std::collections::HashMap<String, u64> {
    let mut counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for line in input.lines() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
            let path = json
                .pointer("/process/executable/path")
                .or_else(|| json.pointer("/event/exec/target/executable/path"))
                .and_then(|v| v.as_str());
            if let Some(path) = path {
                let basename = path.rsplit('/').next().unwrap_or(path);
                *counts.entry(basename.to_string()).or_insert(0) += 1;
            }
        }
    }
    counts
}

#[cfg(target_os = "macos")]
pub fn capture(duration_secs: u64) -> Result<ProcSnapshotResult, String> {
    use std::process::{Command, Stdio};

    let mut child = Command::new("eslogger")
        .args(["exec"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start eslogger: {e}. Try: sudo eslogger exec"))?;

    std::thread::sleep(std::time::Duration::from_secs(duration_secs));
    let _ = child.kill();

    let output = child
        .wait_with_output()
        .map_err(|e| format!("eslogger error: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let counts = parse_eslogger_output(&stdout);
    decide_snapshot(
        duration_secs,
        counts,
        "no process events captured (eslogger may need elevated privileges)",
    )
}

#[cfg(target_os = "linux")]
pub fn capture(duration_secs: u64) -> Result<ProcSnapshotResult, String> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let start = std::time::Instant::now();
    let sample_duration = std::time::Duration::from_secs(duration_secs);

    // Initial scan to establish baseline.
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            if let Ok(name) = entry.file_name().into_string() {
                if name.chars().all(|c| c.is_ascii_digit()) {
                    seen.insert(name);
                }
            }
        }
    }

    // Poll for new processes.
    while start.elapsed() < sample_duration {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Ok(entries) = std::fs::read_dir("/proc") {
            for entry in entries.flatten() {
                if let Ok(pid) = entry.file_name().into_string() {
                    if pid.chars().all(|c| c.is_ascii_digit()) && !seen.contains(&pid) {
                        seen.insert(pid.clone());
                        let comm_path = format!("/proc/{pid}/comm");
                        if let Ok(comm) = std::fs::read_to_string(&comm_path) {
                            let name = comm.trim().to_string();
                            *counts.entry(name).or_insert(0) += 1;
                        }
                    }
                }
            }
        }
    }

    decide_snapshot(
        duration_secs,
        counts,
        "no new processes detected during sampling",
    )
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn capture(_duration_secs: u64) -> Result<ProcSnapshotResult, String> {
    Err("proc snapshot not supported on this platform".to_string())
}
