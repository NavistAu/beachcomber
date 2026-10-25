//! Handler for the `refresh` subcommand.
//!
//! `refresh` is the daemon's `refresh` op and nothing more: it re-executes
//! only the Sources the key demands, each at the instance that answers for the
//! directory, and reads nothing back. A key with no Source behind it — a static namespace, a facet key, an
//! unknown name, a field no Source owns — is the daemon's refusal, printed in
//! its own words. `comb get --force` is the read-and-refresh form; this is
//! the re-execution alone.

use crate::cli::exit::Outcome;
use crate::config::Config;

/// `path` is what `main.rs` already settled: `None` for `--global`, otherwise
/// an absolute path — the one given, or the current directory.
pub fn run_refresh(config: &Config, key: &str, path: Option<&str>) -> Outcome {
    let (socket_path, socket_source) = config.resolve_socket_path_with_source();
    let spawn_no_reap = socket_source.no_reap();

    if let Err(e) = crate::daemon::ensure_daemon(&socket_path, spawn_no_reap) {
        crate::cli::report::daemon_start(e);
        return Outcome::Error;
    }

    match crate::client::Client::new(socket_path).refresh(key, path) {
        Ok(resp) if resp.ok => Outcome::Done,
        // The daemon's own words for a key it refused.
        Ok(resp) => {
            crate::cli::report::error(key, resp.error.unwrap_or_default());
            Outcome::Error
        }
        Err(e) => {
            crate::cli::report::daemon_unreachable(e);
            Outcome::Error
        }
    }
}
