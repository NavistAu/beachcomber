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
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathError::OtherUser(p) => write!(
                f,
                "{p}: only ~ and ~/… are expanded, not ~user; write the absolute path"
            ),
            PathError::NoHome(p) => write!(f, "{p}: HOME is not set"),
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
    fn a_tilde_with_no_home_is_an_error() {
        assert_eq!(
            expand_tilde("~/x", None),
            Err(PathError::NoHome("~/x".into()))
        );
    }
}
