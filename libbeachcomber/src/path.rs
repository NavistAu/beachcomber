//! The paths a caller names, read one way wherever comb takes one.
//!
//! A leading `~` is the caller's home, alone or before a `/`; `~user` is
//! not expanded anywhere, and is an error rather than a relative path that
//! happens to start with a tilde.

use std::path::{Path, PathBuf};

/// Why a path could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathError {
    /// `~user/…`: only the caller's own home is expanded.
    OtherUser(String),
    /// A leading `~` with no home to expand it to.
    NoHome(String),
    /// A relative path, and no current directory to make it absolute against.
    NoCwd(String),
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathError::OtherUser(p) => write!(
                f,
                "{p}: only ~ and ~/… are expanded, not ~user; write the absolute path"
            ),
            PathError::NoHome(p) => write!(f, "{p}: HOME is not set"),
            PathError::NoCwd(e) => write!(f, "current directory: {e}"),
        }
    }
}

impl std::error::Error for PathError {}

/// `p` with a leading `~` (alone, or before a `/`) written as `home`; any
/// other `p` as it is.
pub fn expand_tilde(p: &str, home: Option<&str>) -> Result<PathBuf, PathError> {
    let Some(rest) = p.strip_prefix('~') else {
        return Ok(PathBuf::from(p));
    };
    if !(rest.is_empty() || rest.starts_with('/')) {
        return Err(PathError::OtherUser(p.to_string()));
    }
    let Some(home) = home else {
        return Err(PathError::NoHome(p.to_string()));
    };
    Ok(match rest.trim_start_matches('/') {
        "" => PathBuf::from(home),
        below => Path::new(home).join(below),
    })
}

/// The directory a request names, as the wire carries it: absolute, with a
/// leading `~` expanded ([`expand_tilde`]) and a relative path made absolute
/// against `cwd`, the caller's directory. Naming none is `cwd` itself. `cwd`
/// is read only when it is needed, so an absolute path works from a
/// directory that no longer exists.
///
/// Nothing is canonicalised here: the daemon does that, and maps `$HOME` to
/// the global entry, for every request alike.
pub fn normalise(
    requested: Option<&str>,
    home: Option<&str>,
    cwd: impl FnOnce() -> std::io::Result<PathBuf>,
) -> Result<String, PathError> {
    let here = || cwd().map_err(|e| PathError::NoCwd(e.to_string()));
    let path = match requested {
        Some(p) => {
            let expanded = expand_tilde(p, home)?;
            if expanded.is_relative() {
                here()?.join(expanded)
            } else {
                expanded
            }
        }
        None => here()?,
    };
    Ok(path.to_string_lossy().into_owned())
}

/// [`normalise`] against this process's `$HOME` and current directory: the
/// caller of a CLI command, an SDK or a Rust program.
pub fn normalise_here(requested: Option<&str>) -> Result<String, PathError> {
    normalise(
        requested,
        std::env::var("HOME").ok().as_deref(),
        std::env::current_dir,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leading_tilde_is_home() {
        assert_eq!(expand_tilde("~", Some("/h")), Ok(PathBuf::from("/h")));
        assert_eq!(
            expand_tilde("~/a/b", Some("/h")),
            Ok(PathBuf::from("/h/a/b"))
        );
        assert_eq!(expand_tilde("~/", Some("/h/")), Ok(PathBuf::from("/h/")));
    }

    #[test]
    fn anything_else_is_as_written() {
        for p in ["/abs", "rel/x", "./x", "a/~/b", ""] {
            assert_eq!(expand_tilde(p, Some("/h")), Ok(PathBuf::from(p)), "{p}");
        }
    }

    #[test]
    fn tilde_user_is_an_error() {
        assert_eq!(
            expand_tilde("~bob/x", Some("/h")),
            Err(PathError::OtherUser("~bob/x".into()))
        );
    }

    #[test]
    fn normalise_makes_a_named_directory_absolute() {
        let cwd = || Ok(PathBuf::from("/work"));
        assert_eq!(
            normalise(Some("sub/x"), Some("/h"), cwd).unwrap(),
            "/work/sub/x"
        );
        assert_eq!(normalise(Some("."), Some("/h"), cwd).unwrap(), "/work/.");
        assert_eq!(normalise(Some("~/x"), Some("/h"), cwd).unwrap(), "/h/x");
        assert_eq!(normalise(Some("/abs"), Some("/h"), cwd).unwrap(), "/abs");
        assert_eq!(normalise(None, Some("/h"), cwd).unwrap(), "/work");
    }

    #[test]
    fn normalise_reads_the_cwd_only_when_it_needs_it() {
        let gone = || Err(std::io::Error::other("gone"));
        assert_eq!(normalise(Some("/abs"), None, gone).unwrap(), "/abs");
        assert_eq!(
            normalise(Some("rel"), None, gone),
            Err(PathError::NoCwd("gone".into()))
        );
    }

    #[test]
    fn a_tilde_with_no_home_is_an_error() {
        assert_eq!(
            expand_tilde("~/x", None),
            Err(PathError::NoHome("~/x".into()))
        );
    }
}
