//! Files: `read`, `realpath`, `glob` and `lup`. All read the filesystem, so they are the `io`
//! tier: not on a provider's lines, which run on every read.

use super::{UNDEFINED, is_absent};
use crate::engine::{EngineCtx, op_error};
use minijinja::value::{Kwargs, Value as MjValue};
use minijinja::{Environment, Error};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(super) fn add_read(env: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    let ctx = ctx.clone();
    env.add_filter("read", move |path: MjValue| read(&ctx, &path));
}

pub(super) fn add_realpath(env: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    let ctx = ctx.clone();
    env.add_filter("realpath", move |path: MjValue| realpath(&ctx, &path));
}

pub(super) fn add_glob(env: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    let ctx = ctx.clone();
    env.add_filter("glob", move |pattern: MjValue| glob(&ctx, &pattern));
}

pub(super) fn add_lup(env: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    let ctx = ctx.clone();
    env.add_function("lup", move |name: String, kwargs: Kwargs| {
        lup(&ctx, &name, kwargs)
    });
}

/// `p | read`: the file's text. A relative `p` is against `ctx.cwd`, a
/// leading `~` against `ctx.env["HOME"]`. A missing file is undefined.
fn read(ctx: &EngineCtx, path: &MjValue) -> Result<MjValue, Error> {
    if is_absent(path) {
        return Ok(UNDEFINED);
    }
    let Some(p) = path.as_str() else {
        return Err(op_error("read(): the path must be a string"));
    };
    let full = resolve_path(ctx, p).map_err(|e| op_error(format!("read(): {e}")))?;
    match std::fs::read_to_string(&full) {
        Ok(text) => Ok(MjValue::from(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(UNDEFINED),
        Err(e) => Err(op_error(format!("read(): {}: {e}", full.display()))),
    }
}

/// `pattern | glob`: the paths a shell would expand `pattern` to, sorted.
/// `*`, `?` and `[..]` stay within one component, `**` as a whole component
/// crosses directories, and a leading `.` must be written (so `*` and `**` pass
/// over dotfiles and hidden directories). A relative pattern is against the
/// render's directory and its results are relative to it; `~` is the render's
/// `HOME`, and an absolute pattern gives absolute paths. A trailing `/` keeps
/// directories only. No match is `[]`; a directory that cannot be read is
/// passed over.
///
/// The `glob` crate matches each component's name (`Pattern`); the walk is
/// here, since the crate's own cannot be told to match `.*` while skipping
/// hidden entries for `*`.
fn glob(ctx: &EngineCtx, pattern: &MjValue) -> Result<MjValue, Error> {
    if is_absent(pattern) {
        return Ok(UNDEFINED);
    }
    let Some(p) = pattern.as_str() else {
        return Err(op_error("glob(): the pattern must be a string"));
    };
    let expanded = crate::path::expand_tilde(p, ctx.env.get("HOME").map(String::as_str))
        .map_err(|e| op_error(format!("glob(): {e}")))?;
    let expanded = expanded.to_string_lossy().into_owned();
    let absolute = expanded.starts_with('/');
    let mut parts = Vec::new();
    for c in expanded.split('/').filter(|c| !c.is_empty() && *c != ".") {
        parts.push(if c == "**" {
            Part::Recursive
        } else if c.contains(['*', '?', '[']) {
            Part::Wild(
                glob::Pattern::new(c)
                    .map_err(|e| op_error(format!("glob(): invalid pattern {p:?}: {}", e.msg)))?,
            )
        } else {
            Part::Literal(c.to_string())
        });
    }
    let base = match (&ctx.cwd, absolute) {
        (_, true) => PathBuf::from("/"),
        (Some(cwd), false) => cwd.clone(),
        (None, false) => PathBuf::from("."),
    };
    let mut found = Vec::new();
    walk(&base, &PathBuf::new(), &parts, &mut found);
    let dirs_only = p.ends_with('/');
    let mut out: Vec<String> = found
        .into_iter()
        .filter(|rel| !dirs_only || base.join(rel).is_dir())
        .map(|rel| {
            let shown = if absolute {
                Path::new("/").join(&rel)
            } else {
                rel
            };
            let mut s = shown.to_string_lossy().into_owned();
            if dirs_only {
                s.push('/');
            }
            s
        })
        .collect();
    out.sort();
    out.dedup();
    Ok(MjValue::from_serialize(out))
}

/// One component of a glob pattern.
enum Part {
    Literal(String),
    Wild(glob::Pattern),
    /// `**`: any number of directories, none of them hidden.
    Recursive,
}

const NAMES: glob::MatchOptions = glob::MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: true,
};

/// The names in directory `dir`, hidden or not, that can be read as text.
fn names(dir: &Path) -> Vec<(String, bool)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            // A symlink is not descended by `**`, so a loop cannot run.
            let real_dir = e.file_type().is_ok_and(|t| t.is_dir());
            Some((name, real_dir))
        })
        .collect()
}

/// Match `parts` below `base/rel`, pushing every `rel` that matches the whole.
fn walk(base: &Path, rel: &Path, parts: &[Part], found: &mut Vec<PathBuf>) {
    let Some((part, rest)) = parts.split_first() else {
        if !rel.as_os_str().is_empty() {
            found.push(rel.to_path_buf());
        }
        return;
    };
    let here = base.join(rel);
    match part {
        Part::Literal(name) => {
            if std::fs::symlink_metadata(here.join(name)).is_ok() {
                walk(base, &rel.join(name), rest, found);
            }
        }
        Part::Wild(pattern) => {
            for (name, _) in names(&here) {
                if pattern.matches_with(&name, NAMES) {
                    walk(base, &rel.join(name), rest, found);
                }
            }
        }
        Part::Recursive => {
            if rest.is_empty() {
                // A trailing `**` is everything below, files as well.
                for (name, _) in names(&here)
                    .into_iter()
                    .filter(|(n, _)| !n.starts_with('.'))
                {
                    found.push(rel.join(name));
                }
            } else {
                // Zero directories, then each visible directory below.
                walk(base, rel, rest, found);
            }
            for (name, real_dir) in names(&here) {
                if real_dir && !name.starts_with('.') {
                    walk(base, &rel.join(name), parts, found);
                }
            }
        }
    }
}

/// `p | realpath`: the path with symlinks followed and `.` and `..` resolved,
/// against the render's directory and `HOME` as `read` is. A path that is not
/// there is undefined, as is an empty one.
fn realpath(ctx: &EngineCtx, path: &MjValue) -> Result<MjValue, Error> {
    if super::is_absent_text(path) {
        return Ok(UNDEFINED);
    }
    let Some(p) = path.as_str() else {
        return Err(op_error("realpath(): the path must be a string"));
    };
    let full = resolve_path(ctx, p).map_err(|e| op_error(format!("realpath(): {e}")))?;
    match std::fs::canonicalize(&full) {
        Ok(real) => Ok(MjValue::from(real.to_string_lossy().into_owned())),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(UNDEFINED)
        }
        Err(e) => Err(op_error(format!("realpath(): {}: {e}", full.display()))),
    }
}

/// `p` as the render reads it: a leading `~` is `ctx.env["HOME"]`
/// ([`crate::path::expand_tilde`]), and a relative path is against `ctx.cwd`.
fn resolve_path(ctx: &EngineCtx, p: &str) -> Result<PathBuf, crate::path::PathError> {
    let expanded = crate::path::expand_tilde(p, ctx.env.get("HOME").map(String::as_str))?;
    Ok(match &ctx.cwd {
        Some(cwd) => cwd.join(expanded),
        None => expanded,
    })
}

/// `lup(name, all=false, files=false, dirs=false, follow=true, git=false,
/// root=false)`: liblup's walk from `ctx.cwd`. The nearest path called
/// `name` at or above it, as an absolute string, or `none`; with `all`, a list
/// of every one, nearest first. The walk stops at `ctx.env["HOME"]` when the
/// directory is under it, else at `/`; `git` stops at the repository root
/// (outside one, `none`) and `root` at `/`. `files`/`dirs` match only that
/// kind; `follow=false` matches a symlink itself.
fn lup(ctx: &EngineCtx, name: &str, kwargs: Kwargs) -> Result<MjValue, Error> {
    let flag = |key: &str, default: bool| -> Result<bool, Error> {
        kwargs
            .get::<Option<bool>>(key)
            .map(|v| v.unwrap_or(default))
            .map_err(|_| op_error(format!("lup(): {key}= must be true or false")))
    };
    let all = flag("all", false)?;
    let files = flag("files", false)?;
    let dirs = flag("dirs", false)?;
    let follow = flag("follow", true)?;
    let git = flag("git", false)?;
    let root = flag("root", false)?;
    kwargs.assert_all_used()?;
    if files && dirs {
        return Err(op_error("lup(): files= and dirs= exclude each other"));
    }
    if git && root {
        return Err(op_error("lup(): git= and root= exclude each other"));
    }

    let Some(cwd) = &ctx.cwd else {
        return Ok(UNDEFINED);
    };
    // lup follows `..` on disk, so it starts from the physical directory.
    let Ok(start) = cwd.canonicalize() else {
        return Ok(UNDEFINED);
    };
    let mut query = liblup::Query::new(name.as_bytes());
    query.all = all;
    query.follow = follow;
    query.kind_filter = match (files, dirs) {
        (true, _) => Some(liblup::KindFilter::Files),
        (_, true) => Some(liblup::KindFilter::Dirs),
        _ => None,
    };
    let boundary = if git {
        liblup::Boundary::Git
    } else if root {
        liblup::Boundary::Root
    } else {
        liblup::Boundary::Home(ctx.env.get("HOME").map(String::as_bytes))
    };
    let hits = match liblup::lookup(&query, start.as_os_str().as_bytes(), boundary) {
        Ok(hits) => hits,
        Err(liblup::LupError::NoMatch | liblup::LupError::NotInGitRepo) => return Ok(UNDEFINED),
        Err(liblup::LupError::UsageError(m)) => return Err(op_error(format!("lup(): {m}"))),
        Err(liblup::LupError::Io(e)) => {
            return Err(op_error(format!("lup(): {}: {e}", start.display())));
        }
    };
    let mut paths = hits.into_iter().filter_map(|hit| match hit {
        liblup::Hit::Path(p) => Some(MjValue::from(String::from_utf8_lossy(&p).into_owned())),
        liblup::Hit::Contents(_) => None,
    });
    Ok(if all {
        MjValue::from(paths.collect::<Vec<_>>())
    } else {
        paths.next().unwrap_or(UNDEFINED)
    })
}

#[cfg(test)]
mod tests {
    use crate::engine::EngineCtx;
    use crate::library::testing::{ctx_with, eval, eval_in};
    use std::sync::Arc;

    /// A tempdir tree `<root>/a/.git/HEAD` and `<root>/a/b/c`, with the ctx
    /// rooted at `<root>/a/b/c` and `HOME` set to `<root>`.
    fn repo() -> (tempfile::TempDir, std::path::PathBuf, Arc<EngineCtx>) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("a/.git")).unwrap();
        std::fs::write(root.join("a/.git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::create_dir_all(root.join("a/b/c")).unwrap();
        let cwd = root.join("a/b/c");
        let ctx = ctx_with(&[("HOME", root.to_str().unwrap())], Some(&cwd));
        (dir, root, ctx)
    }

    /// A tree to glob: `a.tf`, `b.tf`, `c.txt`, `.hidden.tf`, `sub/d.tf`,
    /// `sub/deep/e.tf`, `.git/f.tf`, with the ctx rooted at the top and `HOME`
    /// there as well.
    fn tree() -> (tempfile::TempDir, std::path::PathBuf, Arc<EngineCtx>) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for f in [
            "a.tf",
            "b.tf",
            "c.txt",
            ".hidden.tf",
            "sub/d.tf",
            "sub/deep/e.tf",
            ".git/f.tf",
        ] {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "").unwrap();
        }
        let ctx = ctx_with(&[("HOME", root.to_str().unwrap())], Some(&root));
        (dir, root, ctx)
    }

    #[test]
    fn glob_is_sorted_and_as_relative_as_the_pattern() {
        let (_dir, root, ctx) = tree();
        let g = |expr: &str| eval_in(ctx.clone(), expr).unwrap();
        assert_eq!(g("'*.tf' | glob | join(',')"), "a.tf,b.tf");
        assert_eq!(g("'*.tf' | glob | length > 0"), "true");
        assert_eq!(g("'sub/*.tf' | glob | join(',')"), "sub/d.tf");
        assert_eq!(g("'./*.txt' | glob | join(',')"), "c.txt",);
        let abs = format!("{}/*.tf", root.display());
        assert_eq!(
            g(&format!("'{abs}' | glob | join(',')")),
            format!("{0}/a.tf,{0}/b.tf", root.display())
        );
        assert_eq!(
            g("'~/*.txt' | glob | join(',')"),
            format!("{}/c.txt", root.display())
        );
    }

    #[test]
    fn glob_double_star_recurses() {
        let (_dir, _root, ctx) = tree();
        let g = |expr: &str| eval_in(ctx.clone(), expr).unwrap();
        assert_eq!(
            g("'**/*.tf' | glob | join(',')"),
            "a.tf,b.tf,sub/d.tf,sub/deep/e.tf"
        );
        assert_eq!(
            g("'sub/**/*.tf' | glob | join(',')"),
            "sub/d.tf,sub/deep/e.tf"
        );
        assert_eq!(g("'*' | glob | join(',')"), "a.tf,b.tf,c.txt,sub");
        assert_eq!(g("'?.txt' | glob | join(',')"), "c.txt");
        assert_eq!(g("'[ab].tf' | glob | join(',')"), "a.tf,b.tf");
        assert_eq!(
            g("'sub/**' | glob | join(',')"),
            "sub/d.tf,sub/deep,sub/deep/e.tf"
        );
        assert_eq!(g("'**/e.tf' | glob | join(',')"), "sub/deep/e.tf");
        assert_eq!(g("'*/' | glob | join(',')"), "sub/");
        assert_eq!(g("'sub/../c.txt' | glob | join(',')"), "sub/../c.txt");
    }

    #[test]
    fn glob_passes_over_dotfiles_unless_the_dot_is_written() {
        let (_dir, _root, ctx) = tree();
        let g = |expr: &str| eval_in(ctx.clone(), expr).unwrap();
        assert_eq!(g("'.*.tf' | glob | join(',')"), ".hidden.tf");
        assert_eq!(g("'.git/*.tf' | glob | join(',')"), ".git/f.tf");
        assert!(!g("'**/*' | glob | join(',')").contains(".git"));
    }

    #[test]
    fn glob_with_no_match_is_an_empty_list() {
        let (_dir, _root, ctx) = tree();
        let g = |expr: &str| eval_in(ctx.clone(), expr).unwrap();
        assert_eq!(g("'*.nope' | glob | length"), "0");
        assert_eq!(g("'nodir/*' | glob | length"), "0");
        assert_eq!(g("'' | glob | length"), "0");
        assert_eq!(
            g("'c.txt/*' | glob | length"),
            "0",
            "a file has no children"
        );
        assert_eq!(g("('*.nope' | glob) == []"), "true");
    }

    #[test]
    fn glob_reads_the_render_directory_literally() {
        let dir = tempfile::tempdir().unwrap();
        let odd = dir.path().canonicalize().unwrap().join("we[ir]d*dir");
        std::fs::create_dir_all(&odd).unwrap();
        std::fs::write(odd.join("x.tf"), "").unwrap();
        let ctx = ctx_with(&[], Some(&odd));
        assert_eq!(eval_in(ctx, "'*.tf' | glob | join(',')").unwrap(), "x.tf");
    }

    #[test]
    fn glob_of_nothing_is_undefined_and_a_bad_pattern_is_an_error() {
        let (_dir, _root, ctx) = tree();
        let g = |expr: &str| eval_in(ctx.clone(), expr);
        assert_eq!(g("(none | glob) is undefined").unwrap(), "true");
        assert_eq!(g("(nope | glob) is undefined").unwrap(), "true");
        let e = g("'[' | glob").unwrap_err();
        assert!(e.contains("glob(): invalid pattern \"[\""), "{e}");
        let e = g("'***/x' | glob").unwrap_err();
        assert!(e.contains("glob(): invalid pattern"), "{e}");
        let e = g("[1] | glob").unwrap_err();
        assert!(e.contains("glob(): the pattern must be a string"), "{e}");
        let e = g("'~bob/*' | glob").unwrap_err();
        assert!(e.contains("glob(): ~bob/*"), "{e}");
    }

    #[test]
    fn realpath_follows_symlinks_and_resolves_dots() {
        let (_dir, root, ctx) = repo();
        std::os::unix::fs::symlink(root.join("a/b"), root.join("link")).unwrap();
        let real = |expr: &str| eval_in(ctx.clone(), expr).unwrap();
        let want = root.join("a/b").to_string_lossy().into_owned();
        assert_eq!(real("'~/link' | realpath"), want);
        assert_eq!(real("'../c/..' | realpath"), want, "against the render dir");
        assert_eq!(real("'.' | realpath"), root.join("a/b/c").to_string_lossy());
        assert_eq!(
            real("'~/a/b/../.git' | realpath"),
            root.join("a/.git").to_string_lossy()
        );
    }

    #[test]
    fn realpath_of_what_is_not_there_is_undefined() {
        let (_dir, _root, ctx) = repo();
        let real = |expr: &str| eval_in(ctx.clone(), expr).unwrap();
        assert_eq!(real("('nope' | realpath) is undefined"), "true");
        assert_eq!(
            real("('~/a/.git/HEAD/under' | realpath) is undefined"),
            "true"
        );
        assert_eq!(real("('' | realpath) is undefined"), "true");
        assert_eq!(real("(none | realpath) is undefined"), "true");
    }

    #[test]
    fn realpath_reports_other_errors_naming_the_path() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, root, ctx) = repo();
        let locked = root.join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("f"), "").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let denied = eval_in(ctx.clone(), "'~/locked/f' | realpath");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        // Root is let through anywhere; everyone else gets the error.
        if let Err(e) = denied {
            assert!(e.contains("realpath(): ") && e.contains("locked/f"), "{e}");
        }
        let e = eval_in(ctx.clone(), "7 | realpath").unwrap_err();
        assert!(e.contains("realpath(): the path must be a string"), "{e}");
        let e = eval_in(ctx, "'~bob/x' | realpath").unwrap_err();
        assert!(e.contains("realpath(): ~bob/x"), "{e}");
    }

    #[test]
    fn read_relative_to_cwd() {
        let (_d, root, ctx) = repo();
        std::fs::write(root.join("a/b/c/note.txt"), "hello\n").unwrap();
        assert_eq!(eval_in(ctx, "'note.txt' | read").unwrap(), "hello\n");
    }

    #[test]
    fn read_is_a_filter_only() {
        let (_d, root, ctx) = repo();
        std::fs::write(root.join("a/b/c/note.txt"), "hello\n").unwrap();
        let err = eval_in(ctx, "read('note.txt')").unwrap_err();
        assert!(err.contains("unknown function"), "unexpected error: {err}");
    }

    #[test]
    fn read_tilde_uses_env_home() {
        let (_d, root, ctx) = repo();
        std::fs::write(root.join("home.txt"), "h").unwrap();
        assert_eq!(eval_in(ctx, "'~/home.txt' | read").unwrap(), "h");
    }

    #[test]
    fn read_of_tilde_user_is_an_error_not_a_relative_file() {
        let (_d, _root, ctx) = repo();
        let err = eval_in(ctx, "'~bob/x' | read").unwrap_err();
        assert!(err.contains("~bob/x") && err.contains("~user"), "{err}");
    }

    #[test]
    fn read_missing_is_undefined() {
        let (_d, _root, ctx) = repo();
        assert_eq!(
            eval_in(ctx.clone(), "('nope' | read) is undefined").unwrap(),
            "true"
        );
        assert_eq!(
            eval_in(ctx, "('nope' | read) or 'fallback'").unwrap(),
            "fallback"
        );
    }

    #[test]
    fn read_non_utf8_is_an_error_naming_the_path() {
        let (_d, root, ctx) = repo();
        std::fs::write(root.join("a/b/c/bin"), [0xff, 0xfe]).unwrap();
        let err = eval_in(ctx, "'bin' | read").unwrap_err();
        assert!(err.contains("bin"), "unexpected error: {err}");
    }

    #[test]
    fn read_other_io_error_is_an_error_naming_the_path() {
        let (_d, _root, ctx) = repo();
        let err = eval_in(ctx, "'.' | read").unwrap_err();
        assert!(err.contains("read():") && err.contains('.'), "{err}");
    }

    #[test]
    fn lup_finds_the_nearest_path_at_or_above_cwd() {
        let (_d, root, ctx) = repo();
        let want = root.join("a/.git");
        assert_eq!(eval_in(ctx, "lup('.git')").unwrap(), want.to_str().unwrap());
    }

    #[test]
    fn lup_miss_is_undefined() {
        let (_d, _root, ctx) = repo();
        assert_eq!(
            eval_in(ctx, "lup('.nothing-here') is undefined").unwrap(),
            "true"
        );
        assert_eq!(eval("lup('.git') is undefined").unwrap(), "true");
    }

    #[test]
    fn lup_stops_at_home() {
        let (_d, root, _ctx) = repo();
        // HOME is <root>/a/b: the .git in <root>/a is above the boundary.
        let cwd = root.join("a/b/c");
        let home = root.join("a/b");
        let ctx = ctx_with(&[("HOME", home.to_str().unwrap())], Some(&cwd));
        assert_eq!(eval_in(ctx, "lup('.git') is undefined").unwrap(), "true");
    }

    #[test]
    fn lup_all_lists_every_hit_nearest_first() {
        let (_d, root, ctx) = repo();
        std::fs::write(root.join("a/b/.env"), "").unwrap();
        std::fs::write(root.join(".env"), "").unwrap();
        let want = format!(
            "[{:?}, {:?}]",
            root.join("a/b/.env").to_str().unwrap(),
            root.join(".env").to_str().unwrap()
        );
        assert_eq!(eval_in(ctx, "lup('.env', all=true)").unwrap(), want);
    }

    #[test]
    fn lup_dirs_skips_a_nearer_file() {
        let (_d, root, ctx) = repo();
        std::fs::write(root.join("a/b/.git"), "gitdir: elsewhere\n").unwrap();
        assert_eq!(
            eval_in(ctx.clone(), "lup('.git')").unwrap(),
            root.join("a/b/.git").to_str().unwrap()
        );
        assert_eq!(
            eval_in(ctx, "lup('.git', dirs=true)").unwrap(),
            root.join("a/.git").to_str().unwrap()
        );
    }

    #[test]
    fn lup_git_stops_at_the_repository_root() {
        let (_d, root, ctx) = repo();
        std::fs::write(root.join(".env"), "").unwrap();
        assert_eq!(
            eval_in(ctx.clone(), "lup('.env')").unwrap(),
            root.join(".env").to_str().unwrap()
        );
        assert_eq!(
            eval_in(ctx, "lup('.env', git=true) is undefined").unwrap(),
            "true"
        );
    }

    #[test]
    fn lup_git_outside_a_repository_is_undefined() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_with(&[], Some(dir.path()));
        assert_eq!(
            eval_in(ctx, "lup('.env', git=true) is undefined").unwrap(),
            "true"
        );
    }

    #[test]
    fn lup_walks_from_the_physical_directory() {
        let (_d, root, _ctx) = repo();
        let link = root.join("link");
        std::os::unix::fs::symlink(root.join("a/b/c"), &link).unwrap();
        let ctx = ctx_with(&[("HOME", root.to_str().unwrap())], Some(&link));
        assert_eq!(
            eval_in(ctx, "lup('.git')").unwrap(),
            root.join("a/.git").to_str().unwrap()
        );
    }

    #[test]
    fn lup_exclusive_options_are_errors() {
        let (_d, _root, ctx) = repo();
        let err = eval_in(ctx.clone(), "lup('.git', files=true, dirs=true)").unwrap_err();
        assert!(err.contains("files= and dirs="), "unexpected error: {err}");
        let err = eval_in(ctx.clone(), "lup('.git', git=true, root=true)").unwrap_err();
        assert!(err.contains("git= and root="), "unexpected error: {err}");
        let err = eval_in(ctx, "lup('.git', all='yes')").unwrap_err();
        assert!(err.contains("all= must be"), "unexpected error: {err}");
    }

    #[test]
    fn root_line_from_the_spec() {
        let (_d, root, ctx) = repo();
        assert_eq!(
            eval_in(ctx, "lup('.git') | dirname").unwrap(),
            root.join("a").to_str().unwrap()
        );
    }

    #[test]
    fn head_line_from_the_spec() {
        let (_d, _root, ctx) = repo();
        assert_eq!(
            eval_in(ctx, "lup('.git/HEAD') | read | trim").unwrap(),
            "ref: refs/heads/main"
        );
    }
}
