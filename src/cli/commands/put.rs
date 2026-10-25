//! Handler for the `put` subcommand.
//!
//! Moved from `src/main.rs` in Task 2.4.

use crate::cli::exit::Outcome;
use crate::config::Config;

pub fn run_put(
    config: &Config,
    key: &str,
    data_str: &str,
    ttl: Option<std::time::Duration>,
    path: Option<&str>,
) -> Outcome {
    // A ttl is whole seconds of at least one; refused here, before a daemon
    // is started for a put that cannot happen.
    let ttl = match ttl {
        Some(d) if d.subsec_nanos() != 0 || d.is_zero() => {
            crate::cli::report::error("--ttl", "must be a whole number of seconds, at least 1s");
            return Outcome::Error;
        }
        Some(d) => Some(format!("{}s", d.as_secs())),
        None => None,
    };
    let (socket_path, socket_source) = config.resolve_socket_path_with_source();
    let spawn_no_reap = matches!(socket_source, crate::config::SocketPathSource::EnvVar);

    if let Err(e) = crate::daemon::ensure_daemon(&socket_path, spawn_no_reap) {
        crate::cli::report::daemon_start(e);
        return Outcome::Error;
    }

    let client = crate::client::Client::new(socket_path);

    let data: serde_json::Value = match serde_json::from_str(data_str) {
        Ok(v) => v,
        Err(e) => {
            crate::cli::report::error(key, format_args!("invalid JSON: {e}"));
            return Outcome::Error;
        }
    };

    // Whether this JSON is legal here depends on the node the key
    // addresses — an object at the root, anything below it — which only the
    // daemon knows. It reports the mismatch.
    match client.put(key, data, ttl.as_deref(), path) {
        Ok(response) => {
            if response.ok {
                Outcome::Done
            } else {
                crate::cli::report::error(key, response.error.unwrap_or_default());
                Outcome::Error
            }
        }
        Err(e) => {
            crate::cli::report::daemon_unreachable(e);
            Outcome::Error
        }
    }
}
