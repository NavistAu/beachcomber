//! The paths a caller names, read one way wherever comb takes one.
//!
//! A leading `~` is the caller's home, alone or before a `/`; `~user` is
//! not expanded anywhere, and is an error rather than a relative path that
//! happens to start with a tilde.
//!
//! [`Home`] is the other direction: the one rule for displaying a path under
//! `$HOME`, used by `comb status`'s PATH and SOURCE, `check config --origins`,
//! the `config(<file>)` label every record and error carries, and the
//! `shorten_path` template filter, so one directory reads one way everywhere.
//! A path at or below home is written from `~`: home itself is `~`, a path
//! below it `~/rest`. Home is recognised in both the form `$HOME` holds and its
//! canonical form, so a symlinked `$HOME` (`/tmp` -> `/private/tmp` on macOS)
//! compacts whichever form a path arrives in.

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

/// Home, in the two forms a path under it may arrive in.
#[derive(Clone, Debug, Default)]
pub struct Home {
    raw: Option<String>,
    canonical: Option<String>,
}

impl Home {
    /// This process's `$HOME`. An unset or empty `$HOME` compacts nothing.
    pub fn from_env() -> Home {
        Home::of(std::env::var("HOME").ok().as_deref())
    }

    /// Home at `raw` exactly as written, with no look-up on disk: what a pure
    /// template item uses, which may touch no file.
    pub fn literal(raw: Option<&str>) -> Home {
        Home {
            raw: raw
                .filter(|h| !h.is_empty())
                .map(|h| h.trim_end_matches('/').to_string()),
            canonical: None,
        }
    }

    /// Home at `raw`, with its canonical form looked up on disk.
    pub fn of(raw: Option<&str>) -> Home {
        let raw = raw
            .filter(|h| !h.is_empty())
            .map(|h| h.trim_end_matches('/'));
        let canonical = raw
            .and_then(|h| std::fs::canonicalize(h).ok())
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|c| Some(c.as_str()) != raw);
        Home {
            raw: raw.map(str::to_string),
            canonical,
        }
    }

    /// `path` written from `~` when it is home or below it, else as given.
    pub fn compact(&self, path: &str) -> String {
        for home in [&self.raw, &self.canonical].into_iter().flatten() {
            if path == home {
                return "~".to_string();
            }
            if let Some(rest) = path.strip_prefix(home.as_str())
                && rest.starts_with('/')
            {
                return format!("~{rest}");
            }
        }
        path.to_string()
    }

    /// [`Home::compact`] for a filesystem path.
    pub fn compact_path(&self, path: &Path) -> String {
        self.compact(&path.to_string_lossy())
    }
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

#[cfg(test)]
mod home_tests {
    use super::Home;

    #[test]
    fn home_and_below_are_written_from_tilde() {
        let home = Home::of(Some("/Users/x"));
        assert_eq!(home.compact("/Users/x"), "~");
        assert_eq!(home.compact("/Users/x/ws/beachcomber"), "~/ws/beachcomber");
        assert_eq!(home.compact("/var/log"), "/var/log");
    }

    #[test]
    fn a_sibling_sharing_the_prefix_is_not_below_home() {
        let home = Home::of(Some("/Users/joe"));
        assert_eq!(home.compact("/Users/joel/x"), "/Users/joel/x");
    }

    #[test]
    fn no_home_compacts_nothing() {
        assert_eq!(Home::of(None).compact("/Users/x/a"), "/Users/x/a");
        assert_eq!(Home::of(Some("")).compact("/Users/x/a"), "/Users/x/a");
    }

    #[test]
    fn a_symlinked_home_compacts_in_either_form() {
        let dir = tempfile::TempDir::new().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let home = Home::of(Some(link.to_str().unwrap()));
        let canonical = std::fs::canonicalize(&real).unwrap();
        assert_eq!(home.compact(&format!("{}/ws", link.display())), "~/ws");
        assert_eq!(home.compact(&format!("{}/ws", canonical.display())), "~/ws");
    }
}
