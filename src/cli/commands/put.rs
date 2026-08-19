//! Handler for the `put` subcommand.
//!
//! Moved from `src/main.rs` in Task 2.4.

use crate::config::Config;
use std::process::ExitCode;

pub fn run_put(
    config: &Config,
    key: &str,
    data_str: Option<&str>,
    null: bool,
    ttl: Option<&str>,
    path: Option<&str>,
) -> ExitCode {
    // Validate argument combinations.
    if null && data_str.is_some() {
        eprintln!("cannot combine --null with a data argument");
        return ExitCode::from(2);
    }
    if !null && data_str.is_none() {
        eprintln!("put requires either a data argument or --null");
        return ExitCode::from(2);
    }

    let (socket_path, socket_source) = config.resolve_socket_path_with_source();
    let spawn_no_reap = matches!(socket_source, crate::config::SocketPathSource::EnvVar);

    if let Err(e) = crate::daemon::ensure_daemon(&socket_path, spawn_no_reap) {
        eprintln!("Failed to start daemon: {e}");
        return ExitCode::from(2);
    }

    let client = crate::client::Client::new(socket_path);

    if null {
        match client.put_null(key, ttl, path) {
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
    } else {
        let data_str = data_str.unwrap();
        let data: serde_json::Value = match serde_json::from_str(data_str) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Invalid JSON: {e}");
                return ExitCode::from(2);
            }
        };

        if !data.is_object() {
            eprintln!("put data must be a JSON object");
            return ExitCode::from(2);
        }

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
}
