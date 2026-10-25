//! What comb tells the user when something goes wrong: one form for every
//! command, on stderr, the form clap's own usage errors take.
//!
//! - `error: <who>: <why>` — `who` names what the error is about: a key,
//!   `daemon`, a flag, the current directory.
//! - `error: <why>` — for a message that already names its subject, as a
//!   resolver error does (`git.bad: template compile error: …`).
//! - `warning: <why>` — a diagnostic that does not fail the command.

use libbeachcomber::resolve::ResolveError;
use libbeachcomber::watch::WatchError;
use std::fmt::Display;

/// `error: <who>: <why>`.
pub fn error(who: &str, why: impl Display) {
    eprintln!("error: {who}: {why}");
}

/// `error: <message>`, for a message that names its own subject.
pub fn error_named(message: impl Display) {
    eprintln!("error: {message}");
}

/// `warning: <why>`.
pub fn warning(why: impl Display) {
    eprintln!("warning: {why}");
}

/// The daemon could not be started.
pub fn daemon_start(e: impl Display) {
    error("daemon", format_args!("could not start: {e}"));
}

/// The daemon could not be reached once started.
pub fn daemon_unreachable(e: impl Display) {
    error("daemon", e);
}

/// A resolution of `key` failed. A failure to reach the daemon, or the
/// daemon's refusal of the key, is about `key`; any other failure already
/// names the field it is about.
pub fn resolve_error(key: &str, e: &ResolveError) {
    match e {
        ResolveError::Fetch(fetch) => error(key, fetch),
        named => error_named(named),
    }
}

/// A watch on `key` failed: a resolution as [`resolve_error`] reports one,
/// anything else about `key`.
pub fn watch_error(key: &str, e: &WatchError) {
    match e {
        WatchError::Resolve(resolve) => resolve_error(key, resolve),
        other => error(key, other),
    }
}
