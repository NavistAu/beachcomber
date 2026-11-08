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
/// leaves it on the last one it loaded — unless it is an older release than
/// this `comb`, which it is restarted to become ([`restart_if_older`]).
pub fn connect(config: &Config) -> Result<PathBuf, Outcome> {
    connect_inner(config, true)
}

/// [`connect`] that leaves a running daemon as it is, whatever its version:
/// for `status`, which reports the daemon it finds.
pub fn connect_as_is(config: &Config) -> Result<PathBuf, Outcome> {
    connect_inner(config, false)
}

fn connect_inner(config: &Config, restart_older: bool) -> Result<PathBuf, Outcome> {
    let (socket, source) = config.resolve_socket_path_with_source();
    let restarted = if restart_older && libbeachcomber::spawn::is_serving(&socket) {
        restart_if_older(&socket, config)
    } else {
        None
    };
    if libbeachcomber::spawn::is_serving(&socket) {
        return Ok(socket);
    }
    // `comb check config`'s whole check: it builds every provider, so it runs
    // only here, where a daemon is about to be started, never on a read.
    let errors = crate::cli::commands::check::config_failures(config);
    if !errors.is_empty() {
        for line in crate::daemon::refusal(&errors) {
            eprintln!("{line}");
        }
        return Err(Outcome::Error);
    }
    let start = || -> Result<(), libbeachcomber::spawn::Error> {
        let binary = std::env::current_exe().map_err(libbeachcomber::spawn::Error::Io)?;
        let log = Config::log_path();
        let spawn = libbeachcomber::spawn::Spawn {
            binary: &binary,
            no_reap: source.no_reap(),
            log: &log,
        };
        libbeachcomber::spawn::ensure(&socket, &spawn)
    };
    match start() {
        Ok(()) => {
            if let Some(old) = restarted {
                // The new daemon's own word: a spawner resolving to an old
                // binary shows here, and is used as it is — no second try.
                let new = daemon_version(&socket).unwrap_or_else(|| "?".into());
                crate::cli::report::note(format_args!("restarted daemon {old} -> {new}"));
            }
            Ok(socket)
        }
        Err(e) => {
            crate::cli::report::daemon_start(e);
            Err(Outcome::Error)
        }
    }
}

/// Stop the daemon serving `socket` if it reports a release older than this
/// `comb` (a package upgrade left it running), returning the version it had.
/// Only an older one: a newer daemon serves an old pinned `comb` as it is, or
/// two installed versions would knock each other over in turn. Not under a
/// configuration the new daemon would refuse: the old one still serves the
/// last configuration it loaded.
fn restart_if_older(socket: &std::path::Path, config: &Config) -> Option<String> {
    let theirs = daemon_version(socket)?;
    if !older(&theirs, libbeachcomber::VERSION)
        || !crate::cli::commands::check::config_failures(config).is_empty()
    {
        return None;
    }
    let timeout = std::time::Duration::from_secs(5);
    match crate::cli::commands::kill::stop(socket, timeout) {
        Ok(_) => Some(theirs),
        Err(e) => {
            crate::cli::report::warning(format_args!("could not restart daemon {theirs}: {e}"));
            None
        }
    }
}

fn daemon_version(socket: &std::path::Path) -> Option<String> {
    let hello = crate::client::build_client(socket).hello().ok()?;
    Some(hello.daemon_version)
}

/// Whether `theirs` precedes `ours` in semver precedence (build metadata
/// ignored). Either unparseable → `false`.
pub fn older(theirs: &str, ours: &str) -> bool {
    matches!(
        (Semver::parse(theirs), Semver::parse(ours)),
        (Some(a), Some(b)) if a < b
    )
}

/// `MAJOR.MINOR.PATCH[-PRE]`, ordered by semver precedence.
#[derive(PartialEq, Eq)]
struct Semver<'a> {
    core: (u64, u64, u64),
    pre: Option<Vec<&'a str>>,
}

impl<'a> Semver<'a> {
    fn parse(v: &'a str) -> Option<Self> {
        let v = v.split_once('+').map_or(v, |(v, _build)| v);
        let (core, pre) = match v.split_once('-') {
            Some((core, pre)) => (core, Some(pre.split('.').collect::<Vec<_>>())),
            None => (v, None),
        };
        let mut parts = core.split('.').map(|n| n.parse::<u64>().ok());
        let core = (parts.next()??, parts.next()??, parts.next()??);
        if parts.next().is_some() || pre.as_ref().is_some_and(|p| p.iter().any(|i| i.is_empty())) {
            return None;
        }
        Some(Self { core, pre })
    }
}

impl PartialOrd for Semver<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Semver<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        // An identifier: numeric ones compare as numbers, and before alphanumeric ones.
        let ident = |a: &str, b: &str| match (a.parse::<u64>(), b.parse::<u64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            (Ok(_), Err(_)) => Ordering::Less,
            (Err(_), Ok(_)) => Ordering::Greater,
            (Err(_), Err(_)) => a.cmp(b),
        };
        self.core
            .cmp(&other.core)
            .then_with(|| match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                // A pre-release precedes its release.
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => a
                    .iter()
                    .zip(b)
                    .map(|(x, y)| ident(x, y))
                    .find(|o| o.is_ne())
                    .unwrap_or_else(|| a.len().cmp(&b.len())),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::older;

    #[test]
    fn only_a_strictly_older_daemon_is_older() {
        assert!(older("0.10.0", "0.10.1"));
        assert!(older("0.9.9", "0.10.0"));
        assert!(older("1.2.3", "2.0.0"));
        assert!(!older("0.10.1", "0.10.1"), "equal");
        assert!(!older("0.10.2", "0.10.1"), "newer");
        assert!(!older("1.0.0", "0.99.99"), "newer");
    }

    #[test]
    fn build_metadata_is_ignored() {
        assert!(!older("0.10.1+sha.aaaa", "0.10.1+sha.bbbb"));
        assert!(!older("0.10.1", "0.10.1+sha.db75e72a"));
        assert!(older("0.10.0+sha.ffff", "0.10.1+sha.0000"));
    }

    #[test]
    fn prerelease_precedes_its_release() {
        assert!(older("0.11.0-rc.1", "0.11.0"));
        assert!(!older("0.11.0", "0.11.0-rc.1"));
        assert!(older("0.11.0-alpha", "0.11.0-alpha.1"));
        assert!(older("0.11.0-alpha.1", "0.11.0-beta"));
        assert!(
            older("0.11.0-rc.2", "0.11.0-rc.10"),
            "numeric identifiers compare as numbers"
        );
        assert!(
            older("0.11.0-1", "0.11.0-alpha"),
            "numeric before alphanumeric"
        );
        assert!(older("0.10.1", "0.11.0-rc.1"));
    }

    #[test]
    fn unparseable_is_never_older() {
        for garbage in [
            "",
            "dev",
            "0.10",
            "0.10.x",
            "0.10.1.2",
            "v0.10.0",
            "0.10.0-",
            "0.10.0-a..b",
        ] {
            assert!(!older(garbage, "0.10.1"), "{garbage:?} as daemon");
            assert!(!older("0.0.1", garbage), "{garbage:?} as comb");
        }
    }
}
