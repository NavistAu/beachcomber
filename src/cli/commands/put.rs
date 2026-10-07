//! Handler for the `put` subcommand.
//!
//! Moved from `src/main.rs` in Task 2.4.

use crate::config::Config;
use std::process::ExitCode;

pub fn run_put(
    config: &Config,
    key: &str,
    data_str: &str,
    ttl: Option<&str>,
    path: Option<&str>,
) -> ExitCode {
    let (socket_path, socket_source) = config.resolve_socket_path_with_source();
    let spawn_no_reap = matches!(socket_source, crate::config::SocketPathSource::EnvVar);

    if let Err(e) = crate::daemon::ensure_daemon(&socket_path, spawn_no_reap) {
        eprintln!("Failed to start daemon: {e}");
        return ExitCode::from(2);
    }

    let client = crate::client::Client::new(socket_path);

    let data: serde_json::Value = match serde_json::from_str(data_str) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Invalid JSON: {e}");
            return ExitCode::from(2);
        }
    };

    // Whether this JSON is legal here depends on the node the key
    // addresses — an object at the root, anything below it — which only the
    // daemon knows. It reports the mismatch.
    match client.put(key, data, ttl, path) {
        Ok(response) => {
            if response.ok {
                ExitCode::SUCCESS
            } else {
                eprintln!("Error: {}", response.error.unwrap_or_default());
                ExitCode::from(2)
            }
        }
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::from(2)
        }
    }
}
