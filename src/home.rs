//! `$HOME` as comb shows paths under it: one rule for every place a path is
//! displayed — `comb status`'s PATH and SOURCE, `check config --origins`, the
//! `config(<file>)` label every record and error carries — so one directory
//! reads one way everywhere.
//!
//! A path at or below home is written from `~`: home itself is `~`, a path
//! below it `~/rest`. Home is recognised in both the form `$HOME` holds and
//! its canonical form, so a symlinked `$HOME` (`/tmp` → `/private/tmp` on
//! macOS) compacts whichever form a path arrives in.

use std::path::Path;

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
