//! Measuring and fitting text in terminal columns: `width`, `pad` and
//! `truncate`.
//!
//! Every count is display columns ([`crate::term::width`]): East Asian wide
//! and emoji-presentation characters are 2, combining marks 0, and escape
//! sequences (colours, OSC 8 links) 0. There is no character-count variant.

use super::{UNDEFINED, arguments, is_absent, text, whole};
use crate::engine::{EngineCtx, op_error};
use crate::term;
use minijinja::value::{Kwargs, Rest, Value as MjValue};
use minijinja::{Environment, Error};
use std::sync::Arc;

pub(super) fn add_width(jinja: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    jinja.add_filter("width", |value: MjValue| -> Result<MjValue, Error> {
        if is_absent(&value) {
            return Ok(UNDEFINED);
        }
        Ok(MjValue::from(term::width(&text("width", &value)?)))
    });
}

pub(super) fn add_pad(jinja: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    jinja.add_filter(
        "pad",
        |value: MjValue, args: Rest<MjValue>, kwargs: Kwargs| -> Result<MjValue, Error> {
            let (n, align) = pad_args(&args, &kwargs)?;
            if is_absent(&value) {
                return Ok(UNDEFINED);
            }
            Ok(MjValue::from(pad(&text("pad", &value)?, n, align)))
        },
    );
}

pub(super) fn add_truncate(jinja: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    jinja.add_filter(
        "truncate",
        |value: MjValue, args: Rest<MjValue>, kwargs: Kwargs| -> Result<MjValue, Error> {
            let (n, end) = truncate_args(&args, &kwargs)?;
            if is_absent(&value) {
                return Ok(UNDEFINED);
            }
            Ok(MjValue::from(term::cut(
                &text("truncate", &value)?,
                n,
                &end,
            )))
        },
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Align {
    Left,
    Right,
    Center,
}

/// `pad(n, align='left')`: `n` first, then `align`, either by name.
fn pad_args(args: &[MjValue], kwargs: &Kwargs) -> Result<(i64, Align), Error> {
    let [n, align] = arguments("pad", args, kwargs, ["n", "align"])?;
    let n = n.ok_or_else(|| op_error("pad(): takes a width and an optional align"))?;
    let n = whole("pad", "the width", &n)?;
    let align = match align.as_ref() {
        None => Align::Left,
        Some(a) => match a.as_str() {
            Some("left") => Align::Left,
            Some("right") => Align::Right,
            Some("center") => Align::Center,
            _ => return Err(op_error("pad(): align must be left, right or center")),
        },
    };
    Ok((n, align))
}

/// `s` padded with spaces to `n` columns. `center` puts the floor of half the
/// gap on the left. Wider text, or a negative `n`, is returned unchanged.
fn pad(s: &str, n: i64, align: Align) -> String {
    let have = term::width(s);
    let Some(gap) = usize::try_from(n).ok().and_then(|n| n.checked_sub(have)) else {
        return s.to_string();
    };
    let (left, right) = match align {
        Align::Left => (0, gap),
        Align::Right => (gap, 0),
        Align::Center => (gap / 2, gap - gap / 2),
    };
    format!("{}{s}{}", " ".repeat(left), " ".repeat(right))
}

/// `truncate(n, end='…')`: `n` first, then `end`, either by name.
fn truncate_args(args: &[MjValue], kwargs: &Kwargs) -> Result<(usize, String), Error> {
    let [n, end] = arguments("truncate", args, kwargs, ["n", "end"])?;
    let n = n.ok_or_else(|| op_error("truncate(): takes a length and an optional end"))?;
    let n = whole("truncate", "the length", &n)?;
    let n =
        usize::try_from(n).map_err(|_| op_error("truncate(): the length must not be negative"))?;
    let end = match end.as_ref() {
        None => "…".to_string(),
        Some(e) if is_absent(e) => "…".to_string(),
        Some(e) => text("truncate", e)?,
    };
    Ok((n, end))
}

#[cfg(test)]
mod tests {
    use crate::library::testing::eval;

    #[test]
    fn width_is_display_columns() {
        assert_eq!(eval("'💭 x' | width").unwrap(), "4");
        assert_eq!(eval("'日本' | width").unwrap(), "4");
        assert_eq!(eval("'\\u001b[31mab\\u001b[0m' | width").unwrap(), "2");
        assert_eq!(eval("12 | width").unwrap(), "2");
        assert_eq!(eval("(none | width) is undefined").unwrap(), "true");
        assert!(
            eval("[1] | width")
                .unwrap_err()
                .contains("width(): expected text")
        );
    }

    #[test]
    fn pad_aligns_in_columns() {
        assert_eq!(eval("'ab' | pad(5)").unwrap(), "ab   ");
        assert_eq!(eval("'ab' | pad(5, 'right')").unwrap(), "   ab");
        assert_eq!(eval("'ab' | pad(5, align='right')").unwrap(), "   ab");
        assert_eq!(
            eval("'4h12m' | pad(20, 'center')").unwrap(),
            format!("{}4h12m{}", " ".repeat(7), " ".repeat(8))
        );
        // Wide characters count two.
        assert_eq!(eval("'日' | pad(4)").unwrap(), "日  ");
        // Escapes count none.
        assert_eq!(
            eval("'\\u001b[1mx\\u001b[0m' | pad(3, 'right')").unwrap(),
            "  \x1b[1mx\x1b[0m"
        );
    }

    #[test]
    fn pad_leaves_wider_text_and_negative_widths_alone() {
        assert_eq!(eval("'abcdef' | pad(3)").unwrap(), "abcdef");
        assert_eq!(eval("'ab' | pad(-2)").unwrap(), "ab");
        assert_eq!(eval("'ab' | pad(0, 'center')").unwrap(), "ab");
    }

    #[test]
    fn pad_checks_its_arguments_first() {
        for bad in [
            "'x' | pad(2.5)",
            "'x' | pad('3')",
            "'x' | pad(3, 'middle')",
            "'x' | pad(3, 1)",
            "'x' | pad",
            "'x' | pad(3, 'left', 'left')",
            "'x' | pad(3, 'left', align='left')",
            "none | pad(3, 'middle')",
            "missing | pad('x')",
        ] {
            let err = eval(bad).unwrap_err();
            assert!(err.contains("pad():"), "`{bad}` gave `{err}`");
        }
        assert_eq!(eval("(none | pad(3)) is undefined").unwrap(), "true");
        assert_eq!(eval("(missing | pad(3)) is undefined").unwrap(), "true");
        assert_eq!(eval("7 | pad(3, 'right')").unwrap(), "  7");
    }

    #[test]
    fn truncate_counts_columns_and_fits_the_end_inside() {
        assert_eq!(eval("'abcdef' | truncate(4)").unwrap(), "abc…");
        assert_eq!(eval("'abcdef' | truncate(6)").unwrap(), "abcdef");
        assert_eq!(eval("'abcdef' | truncate(4, '')").unwrap(), "abcd");
        assert_eq!(eval("'abcdef' | truncate(4, end='>')").unwrap(), "abc>");
        assert_eq!(eval("'abcdef' | truncate(2, '...')").unwrap(), "ab");
        assert_eq!(eval("'日本語' | truncate(4)").unwrap(), "日…");
    }

    #[test]
    fn truncate_keeps_escapes() {
        assert_eq!(
            eval("'\\u001b[31mabcdef\\u001b[0m' | truncate(4)").unwrap(),
            "\x1b[31mabc…\x1b[0m"
        );
    }

    #[test]
    fn truncate_checks_its_arguments_first() {
        for bad in [
            "'x' | truncate(-1)",
            "'x' | truncate(1.5)",
            "'x' | truncate('2')",
            "'x' | truncate",
            "none | truncate(-1)",
            "[1] | truncate(3)",
        ] {
            let err = eval(bad).unwrap_err();
            assert!(err.contains("truncate():"), "`{bad}` gave `{err}`");
        }
        assert_eq!(eval("(none | truncate(3)) is undefined").unwrap(), "true");
    }
}
