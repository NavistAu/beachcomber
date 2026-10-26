//! Files: `read`, `realpath` and `lup`. All read the filesystem, so they are the `io`
//! tier: not on a provider's lines, which run on every read.

use super::{UNDEFINED, is_absent};
use crate::engine::{EngineCtx, op_error};
use minijinja::value::{Kwargs, Value as MjValue};
use minijinja::{Environment, Error};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::Arc;

pub(super) fn add_read(env: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    let ctx = ctx.clone();
    env.add_filter("read", move |path: MjValue| read(&ctx, &path));
}

pub(super) fn add_realpath(env: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    let ctx = ctx.clone();
    env.add_filter("realpath", move |path: MjValue| realpath(&ctx, &path));
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
