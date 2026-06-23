use std::collections::HashMap;
use std::path::Path;
use std::process::Output;

/// Boundary trait for running git subprocesses.
///
/// The real implementation wires the standard daemon-safe env vars
/// (`GIT_OPTIONAL_LOCKS=0`, `GIT_TERMINAL_PROMPT=0`, etc.) and sets
/// `current_dir`. Test doubles can return canned output without spawning
/// any real process.
pub trait GitExecutor: Send + Sync {
    /// Run a git command in `dir` with the given args.
    ///
    /// The real implementation also injects daemon-safe env vars; test
    /// implementations may ignore `dir` and return canned output.
    fn run_git(&self, dir: &Path, args: Vec<String>) -> std::io::Result<Output>;
}

pub struct RealGitExecutor;

impl GitExecutor for RealGitExecutor {
    fn run_git(&self, dir: &Path, args: Vec<String>) -> std::io::Result<Output> {
        let mut envs: HashMap<&str, &str> = HashMap::new();
        envs.insert("GIT_OPTIONAL_LOCKS", "0");
        envs.insert("GIT_TERMINAL_PROMPT", "0");
        envs.insert("GIT_ASKPASS", "true");
        envs.insert("SSH_ASKPASS", "true");
        envs.insert("GCM_INTERACTIVE", "Never");

        std::process::Command::new("git")
            .args(&args)
            // Resolve the repo purely from `current_dir`, so subprocess results
            // agree with the daemon's own file-path resolution (resolve_git_dir).
            // An inherited GIT_DIR/GIT_COMMON_DIR/GIT_WORK_TREE would point git at
            // a different repo than the file-read helpers use — a split-brain.
            .env_remove("GIT_DIR")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_WORK_TREE")
            .envs(envs)
            .current_dir(dir)
            .output()
    }
}
