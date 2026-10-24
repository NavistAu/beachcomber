//! Handler for the `refresh` subcommand.
//!
//! `refresh` is the daemon's `refresh` op and nothing more: it re-executes
//! only the Sources the key demands, each at the instance that answers for the
//! directory, and reads nothing back. A key with no Source behind it — a static namespace, a facet key, an
//! unknown name, a field no Source owns — is the daemon's refusal, printed in
//! its own words. `comb get --force` is the read-and-refresh form; this is
//! the re-execution alone.

use crate::config::Config;
use std::process::ExitCode;

/// `path` is what `main.rs` already settled: `None` for `--global`, otherwise
/// an absolute path — the one given, or the current directory.
pub fn run_refresh(config: &Config, key: &str, path: Option<&str>) -> ExitCode {
    let (socket_path, socket_source) = config.resolve_socket_path_with_source();
    let spawn_no_reap = matches!(socket_source, crate::config::SocketPathSource::EnvVar);

    if let Err(e) = crate::daemon::ensure_daemon(&socket_path, spawn_no_reap) {
        eprintln!("Failed to start daemon: {e}");
        return ExitCode::from(2);
    }

    match crate::client::Client::new(socket_path).refresh(key, path) {
        Ok(resp) if resp.ok => ExitCode::SUCCESS,
        // The daemon's own words for a key it refused.
        Ok(resp) => {
            eprintln!("{}", resp.error.unwrap_or_default());
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::from(2)
        }
    }
}
