//! Path text: `basename` and `dirname`. They work on the string alone and
//! touch no file.

use super::{is_blank, none};
use crate::engine::{EngineCtx, op_error};
use minijinja::value::Value as MjValue;
use minijinja::{Environment, Error};
use std::sync::Arc;

pub(super) fn add_basename(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("basename", basename_filter);
}

pub(super) fn add_dirname(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("dirname", dirname);
}

fn basename_filter(value: String) -> String {
    // Remove trailing slashes, then take the last path component.
    let trimmed = value.trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    trimmed.rsplit('/').next().unwrap_or(trimmed).to_string()
}

/// `p | dirname`: all but the last component; `none` stays `none`.
fn dirname(path: MjValue) -> Result<MjValue, Error> {
    if is_blank(&path) {
        return Ok(none());
    }
    let Some(p) = path.as_str() else {
        return Err(op_error("dirname(): the path must be a string"));
    };
    let trimmed = p.trim_end_matches('/');
    let dir = match trimmed.rsplit_once('/') {
        Some(("", _)) => "/",
        Some((dir, _)) => dir,
        None => "",
    };
    Ok(MjValue::from(dir))
}

#[cfg(test)]
mod tests {
    use crate::library::testing::eval;

    #[test]
    fn dirname_forms() {
        assert_eq!(eval("'/a/b/c.txt' | dirname").unwrap(), "/a/b");
        assert_eq!(eval("'c.txt' | dirname").unwrap(), "");
        assert_eq!(eval("'/c' | dirname").unwrap(), "/");
        assert_eq!(eval("'/a/b/' | dirname").unwrap(), "/a");
        assert_eq!(eval("(none | dirname) is none").unwrap(), "true");
    }
}
