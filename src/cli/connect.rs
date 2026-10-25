//! The one step every command that talks to the daemon takes first.

use crate::cli::exit::Outcome;
use crate::config::Config;
use std::path::PathBuf;

/// The daemon's socket, with a daemon serving it. When none is running, the
/// configuration is checked first — a configuration the daemon would refuse
/// is reported here, rather than a daemon spawned only to time out waiting
/// for it — and one is started: this binary, its stderr in the daemon's log
/// ([`libbeachcomber::spawn::ensure`], the spawn every SDK makes too). A
/// running daemon is used as it is: a configuration edited to be invalid
/// leaves it on the last one it loaded.
pub fn connect(config: &Config) -> Result<PathBuf, Outcome> {
    let (socket, source) = config.resolve_socket_path_with_source();
    if libbeachcomber::spawn::is_serving(&socket) {
        return Ok(socket);
    }
    let errors = config.errors();
    if !errors.is_empty() {
        for line in crate::daemon::refusal(&errors) {
            eprintln!("{line}");
        }
        return Err(Outcome::Error);
    }
    let start = || -> std::io::Result<()> {
        let binary = std::env::current_exe()?;
        let log = Config::log_path();
        let spawn = libbeachcomber::spawn::Spawn {
            binary: &binary,
            no_reap: source.no_reap(),
            log: &log,
        };
        libbeachcomber::spawn::ensure(&socket, &spawn)
    };
    match start() {
        Ok(()) => Ok(socket),
        Err(e) => {
            crate::cli::report::daemon_start(e);
            Err(Outcome::Error)
        }
    }
}
