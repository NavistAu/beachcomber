//! Path text: `basename`, `dirname`, `expanduser`, `ext`, `relpath` and
//! `shorten_path`. They work on the string alone (and the render's `HOME` and
//! directory) and touch no file; `realpath` is in [`super::fs`].

use super::{UNDEFINED, is_absent, is_absent_text, text};
use crate::engine::{EngineCtx, op_error};
use minijinja::value::Value as MjValue;
use minijinja::{Environment, Error};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

pub(super) fn add_expanduser(env: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    let ctx = ctx.clone();
    env.add_filter("expanduser", move |path: MjValue| expanduser(&ctx, &path));
}

pub(super) fn add_ext(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("ext", ext);
}

pub(super) fn add_relpath(env: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    let ctx = ctx.clone();
    env.add_filter("relpath", move |path: MjValue, base: Option<MjValue>| {
        relpath(&ctx, &path, base)
    });
}

pub(super) fn add_shorten_path(env: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    let ctx = ctx.clone();
    env.add_filter("shorten_path", move |path: MjValue, n: Option<MjValue>| {
        shorten_path(&ctx, &path, n)
    });
}

fn home(ctx: &EngineCtx) -> Option<&str> {
    ctx.env.get("HOME").map(String::as_str)
}

pub(super) fn add_basename(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("basename", basename);
}

pub(super) fn add_dirname(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("dirname", dirname);
}

/// `p | basename`, POSIX: the last component, trailing slashes ignored; a path
/// of slashes alone is `/`.
fn basename(path: MjValue) -> Result<MjValue, Error> {
    if is_absent_text(&path) {
        return Ok(UNDEFINED);
    }
    let p = text("basename", &path)?;
    let trimmed = p.trim_end_matches('/');
    if trimmed.is_empty() {
        return Ok(MjValue::from("/"));
    }
    Ok(MjValue::from(trimmed.rsplit('/').next().unwrap_or(trimmed)))
}

/// `p | dirname`, POSIX: all but the last component; `c.txt` is `.`, and a
/// path of slashes alone is `/`.
fn dirname(path: MjValue) -> Result<MjValue, Error> {
    if is_absent_text(&path) {
        return Ok(UNDEFINED);
    }
    let p = text("dirname", &path)?;
    let trimmed = p.trim_end_matches('/');
    if trimmed.is_empty() {
        return Ok(MjValue::from("/"));
    }
    let dir = match trimmed.rsplit_once('/') {
        None => ".",
        Some((dir, _)) => match dir.trim_end_matches('/') {
            "" => "/",
            dir => dir,
        },
    };
    Ok(MjValue::from(dir))
}

/// `p | expanduser`: a leading `~` is the render's `HOME`. `~user` is an error,
/// and so is `~` when there is no `HOME`.
fn expanduser(ctx: &EngineCtx, path: &MjValue) -> Result<MjValue, Error> {
    if is_absent_text(path) {
        return Ok(UNDEFINED);
    }
    let p = text("expanduser", path)?;
    let expanded = crate::path::expand_tilde(&p, home(ctx))
        .map_err(|e| op_error(format!("expanduser(): {e}")))?;
    Ok(MjValue::from(expanded.to_string_lossy().into_owned()))
}

/// `p | ext`: the extension of the last component, without its dot. No
/// extension, or a dotfile (`.bashrc`), is undefined.
fn ext(path: MjValue) -> Result<MjValue, Error> {
    if is_absent_text(&path) {
        return Ok(UNDEFINED);
    }
    let p = text("ext", &path)?;
    Ok(Path::new(&p)
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| !e.is_empty())
        .map_or(UNDEFINED, MjValue::from))
}

/// `p` as an absolute, lexically clean list of components: `~` expanded, a
/// relative path taken against the render's directory, `.` dropped and `..`
/// popped. Nothing is read from disk.
fn absolute(ctx: &EngineCtx, p: &str) -> Result<Vec<String>, Error> {
    let expanded =
        crate::path::expand_tilde(p, home(ctx)).map_err(|e| op_error(format!("relpath(): {e}")))?;
    let full = if expanded.is_absolute() {
        expanded
    } else {
        match &ctx.cwd {
            Some(cwd) => cwd.join(expanded),
            None => {
                return Err(op_error(format!(
                    "relpath(): {p} is relative and there is no directory to take it against"
                )));
            }
        }
    };
    let mut parts: Vec<String> = Vec::new();
    for c in full.components() {
        match c {
            Component::Normal(n) => parts.push(n.to_string_lossy().into_owned()),
            Component::ParentDir => {
                parts.pop();
            }
            _ => {}
        }
    }
    Ok(parts)
}

/// `p | relpath(base)`: `p` written relative to `base`, lexically (symlinks are
/// not followed): `'/a/x' | relpath('/a/b')` is `../x`. The same place is `.`.
/// Relative operands are against the render's directory.
fn relpath(ctx: &EngineCtx, path: &MjValue, base: Option<MjValue>) -> Result<MjValue, Error> {
    let base = match base.filter(|b| !is_absent(b)) {
        Some(b) => text("relpath", &b)?,
        None => return Err(op_error("relpath(): a base path is required")),
    };
    if is_absent(path) {
        return Ok(UNDEFINED);
    }
    let target = absolute(ctx, &text("relpath", path)?)?;
    let from = absolute(ctx, &base)?;
    let common = target.iter().zip(&from).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..from.len() {
        out.push("..");
    }
    out.extend(&target[common..]);
    Ok(MjValue::from(if out.as_os_str().is_empty() {
        ".".to_string()
    } else {
        out.to_string_lossy().into_owned()
    }))
}

/// A component cut to its first character; a leading `.` keeps the character
/// after it, and `.` and `..` stay as they are.
fn abbreviate(component: &str) -> String {
    let keep = if component.starts_with('.') { 2 } else { 1 };
    if component == "." || component == ".." {
        return component.to_string();
    }
    component.chars().take(keep).collect()
}

/// `p | shorten_path(n=1)`: home written as `~` (the one display rule,
/// [`crate::path::Home`], against the render's `HOME`), then every component
/// but the last `n` cut to its first character.
fn shorten_path(ctx: &EngineCtx, path: &MjValue, n: Option<MjValue>) -> Result<MjValue, Error> {
    let keep = match n.filter(|n| !is_absent(n)) {
        None => 1,
        Some(n) => super::number("shorten_path", "n", &n)
            .ok()
            .filter(|n| n.fract() == 0.0 && *n >= 0.0)
            .map(|n| n as usize)
            .ok_or_else(|| op_error(format!("shorten_path(): n {n} is not an integer from 0")))?,
    };
    if is_absent(path) {
        return Ok(UNDEFINED);
    }
    let p = text("shorten_path", path)?;
    let shown = crate::path::Home::literal(home(ctx)).compact(&p);
    let parts: Vec<&str> = shown.split('/').collect();
    // The components that can be cut: not the empty ones a leading, doubled or
    // trailing `/` makes, and not the `~` that stands for home.
    let cuttable: Vec<usize> = (0..parts.len())
        .filter(|&i| !(parts[i].is_empty() || i == 0 && parts[0] == "~"))
        .collect();
    let cut = cuttable.len().saturating_sub(keep);
    let out: Vec<String> = parts
        .iter()
        .enumerate()
        .map(|(i, part)| match cuttable.iter().position(|&c| c == i) {
            Some(rank) if rank < cut => abbreviate(part),
            _ => part.to_string(),
        })
        .collect();
    Ok(MjValue::from(out.join("/")))
}

#[cfg(test)]
mod tests {
    use crate::library::testing::{ctx_with, eval, eval_in};
    use std::path::Path;

    fn at(env: &[(&str, &str)], cwd: Option<&str>, expr: &str) -> Result<String, String> {
        eval_in(ctx_with(env, cwd.map(Path::new)), expr)
    }

    fn ok(expr: &str) -> String {
        at(&[("HOME", "/Users/j")], Some("/w/proj"), expr).unwrap_or_else(|e| panic!("{expr}: {e}"))
    }

    fn err(expr: &str) -> String {
        at(&[("HOME", "/Users/j")], Some("/w/proj"), expr).unwrap_err()
    }

    #[test]
    fn expanduser_writes_a_leading_tilde_as_home() {
        assert_eq!(ok("'~/.kube/config' | expanduser"), "/Users/j/.kube/config");
        assert_eq!(ok("'~' | expanduser"), "/Users/j");
        assert_eq!(ok("'/abs/~/x' | expanduser"), "/abs/~/x");
        assert_eq!(ok("'rel/x' | expanduser"), "rel/x");
        assert_eq!(ok("('' | expanduser) is undefined"), "true");
        assert_eq!(ok("(none | expanduser) is undefined"), "true");
    }

    #[test]
    fn expanduser_rejects_tilde_user_and_a_missing_home() {
        let e = err("'~bob/x' | expanduser");
        assert!(e.contains("expanduser(): ~bob/x"), "{e}");
        let e = at(&[], None, "'~/x' | expanduser").unwrap_err();
        assert!(e.contains("expanduser(): ~/x: HOME is not set"), "{e}");
        let e = err("[1] | expanduser");
        assert!(e.contains("expanduser(): expected text"), "{e}");
    }

    #[test]
    fn ext_is_the_last_extension_without_its_dot() {
        assert_eq!(ok("'a.tar.gz' | ext"), "gz");
        assert_eq!(ok("'/x/y/main.rs' | ext"), "rs");
        assert_eq!(ok("'/x.d/readme' | ext is undefined"), "true");
        assert_eq!(ok("'.bashrc.bak' | ext"), "bak");
        for none in ["'Makefile'", "'.bashrc'", "'a.'", "''", "none", "'/x/dir/'"] {
            assert_eq!(
                ok(&format!("({none} | ext) is undefined")),
                "true",
                "{none}"
            );
        }
        let e = err("{} | ext");
        assert!(e.contains("ext(): expected text"), "{e}");
    }

    #[test]
    fn relpath_is_lexical() {
        for (p, base, want) in [
            ("/a/x", "/a/b", "../x"),
            ("/a/b/c", "/a/b", "c"),
            ("/a/b", "/a/b", "."),
            ("/a", "/a/b/c", "../.."),
            ("/x/y", "/a/b", "../../x/y"),
            ("/a/./b/../x", "/a/b", "../x"),
            ("/a/b/", "/a", "b"),
        ] {
            assert_eq!(
                ok(&format!("'{p}' | relpath('{base}')")),
                want,
                "{p} {base}"
            );
        }
    }

    #[test]
    fn relpath_takes_relative_operands_against_the_render_directory() {
        assert_eq!(ok("'x' | relpath('/w')"), "proj/x");
        assert_eq!(ok("'/w/other' | relpath('.')"), "../other");
        assert_eq!(ok("'~/a' | relpath('~')"), "a");
    }

    #[test]
    fn relpath_rejects_missing_base_and_a_relative_path_with_no_directory() {
        let e = err("'/a' | relpath");
        assert!(e.contains("relpath(): a base path is required"), "{e}");
        let e = err("'/a' | relpath(none)");
        assert!(e.contains("relpath(): a base path is required"), "{e}");
        let e = at(&[], None, "'x' | relpath('/a')").unwrap_err();
        assert!(e.contains("relpath(): x is relative"), "{e}");
        let e = at(&[], None, "'/a' | relpath('b')").unwrap_err();
        assert!(e.contains("relpath(): b is relative"), "{e}");
        assert_eq!(at(&[], None, "'/a/x' | relpath('/a')").unwrap(), "x");
        assert_eq!(ok("(none | relpath('/a')) is undefined"), "true");
        let e = err("none | relpath");
        assert!(
            e.contains("relpath(): a base path is required"),
            "arguments first: {e}"
        );
    }

    #[test]
    fn shorten_path_cuts_all_but_the_last_n_components() {
        for (p, n, want) in [
            ("/Users/j/ws/beachcomber/docs", 2, "~/w/beachcomber/docs"),
            ("/Users/j/ws/beachcomber/docs", 1, "~/w/b/docs"),
            ("/Users/j/ws/beachcomber/docs", 0, "~/w/b/d"),
            ("/Users/j/ws/beachcomber/docs", 9, "~/ws/beachcomber/docs"),
            ("/Users/j", 1, "~"),
            ("/Users/j/.config/nvim/init.lua", 1, "~/.c/n/init.lua"),
            ("/usr/local/bin", 1, "/u/l/bin"),
            ("/usr/local/bin/", 1, "/u/l/bin/"),
            ("/", 1, "/"),
            ("rel/path/here", 1, "r/p/here"),
            ("../up/dir", 1, "../u/dir"),
            ("/Users/jx/proj", 1, "/U/j/proj"),
            ("/ü/éa/b", 1, "/ü/é/b"),
        ] {
            assert_eq!(ok(&format!("'{p}' | shorten_path({n})")), want, "{p} n={n}");
        }
        assert_eq!(
            ok("'/Users/j/ws/beachcomber/docs' | shorten_path"),
            "~/w/b/docs"
        );
    }

    #[test]
    fn shorten_path_rejects_a_bad_n_and_reads_absent_as_undefined() {
        for bad in ["-1", "1.5", "'a'"] {
            let e = err(&format!("'/a' | shorten_path({bad})"));
            assert!(e.contains("shorten_path(): n"), "{bad}: {e}");
        }
        assert_eq!(ok("(none | shorten_path) is undefined"), "true");
        assert_eq!(ok("'' | shorten_path"), "");
        let e = err("[1] | shorten_path");
        assert!(e.contains("shorten_path(): expected text"), "{e}");
        assert_eq!(
            at(&[], None, "'/Users/j/ws/x' | shorten_path").unwrap(),
            "/U/j/w/x",
            "no HOME compacts nothing"
        );
    }

    #[test]
    fn basename_and_dirname_follow_posix() {
        for (path, base, dir) in [
            ("/a/b/c.txt", "c.txt", "/a/b"),
            ("c.txt", "c.txt", "."),
            ("/c", "c", "/"),
            ("/a/b/", "b", "/a"),
            ("/", "/", "/"),
            ("//", "/", "/"),
            ("a//b", "b", "a"),
            ("a/b", "b", "a"),
            ("/a/b//", "b", "/a"),
        ] {
            assert_eq!(
                eval(&format!("'{path}' | basename")).unwrap(),
                base,
                "{path}"
            );
            assert_eq!(eval(&format!("'{path}' | dirname")).unwrap(), dir, "{path}");
        }
        assert_eq!(eval("(none | dirname) is undefined").unwrap(), "true");
        assert_eq!(eval("('' | basename) is undefined").unwrap(), "true");
        assert_eq!(eval("(7 | basename)").unwrap(), "7");
        let err = eval("[1] | dirname").unwrap_err();
        assert!(err.contains("dirname(): expected text"), "{err}");
    }
}
