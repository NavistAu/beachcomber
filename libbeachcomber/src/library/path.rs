//! Path text: `basename` and `dirname`. They work on the string alone and
//! touch no file.

use super::{UNDEFINED, is_absent_text, text};
use crate::engine::EngineCtx;
use minijinja::value::Value as MjValue;
use minijinja::{Environment, Error};
use std::sync::Arc;

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

#[cfg(test)]
mod tests {
    use crate::library::testing::eval;

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
