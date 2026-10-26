//! Terminal styling: the colour filters `fg` and `bg`, the
//! attributes `bold`, `dim`, `italic`, `underline` and `invert`, and
//! `hyperlink`.
//!
//! All are pure string transforms (no filesystem, network or process access),
//! so unlike `exec()` they need no `allow_exec` gating and are on every
//! surface. A colour is a name, a palette index or `#rrggbb` ([`crate::term`]),
//! drawn at the depth the render's `env` says its terminal has. With
//! `NO_COLOR` set to something there ([`EngineCtx::colour`]) no item emits an
//! escape sequence.

use super::{UNDEFINED, is_absent, text};
use crate::engine::{EngineCtx, op_error};
use crate::term::{Colour, Depth, Ground, sgr};
use minijinja::value::{Value as MjValue, ValueKind};
use minijinja::{Environment, Error};
use std::sync::Arc;

/// Run `style` over the text of `value`; absent is undefined.
fn styled(
    name: &str,
    value: &MjValue,
    style: impl FnOnce(String) -> Result<String, Error>,
) -> Result<MjValue, Error> {
    if is_absent(value) {
        return Ok(UNDEFINED);
    }
    style(text(name, value)?).map(MjValue::from)
}

pub(super) fn add_fg(jinja: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    add_ground(jinja, ctx, "fg", Ground::Fore);
}

pub(super) fn add_bg(jinja: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    add_ground(jinja, ctx, "bg", Ground::Back);
}

fn add_ground(
    jinja: &mut Environment<'_>,
    ctx: &Arc<EngineCtx>,
    name: &'static str,
    ground: Ground,
) {
    let (on, depth) = (ctx.colour(), ctx.depth());
    jinja.add_filter(name, move |value: MjValue, colour: MjValue| {
        // The colour is checked first, so an empty or absent subject does not
        // hide a typo.
        let colour = colour_arg(name, &colour)?;
        styled(name, &value, |s| Ok(paint(&s, colour, ground, depth, on)))
    });
}

pub(super) fn add_bold(jinja: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    add_attribute(jinja, ctx, "bold", 1);
}

pub(super) fn add_dim(jinja: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    add_attribute(jinja, ctx, "dim", 2);
}

pub(super) fn add_italic(jinja: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    add_attribute(jinja, ctx, "italic", 3);
}

pub(super) fn add_underline(jinja: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    add_attribute(jinja, ctx, "underline", 4);
}

pub(super) fn add_invert(jinja: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    add_attribute(jinja, ctx, "invert", 7);
}

fn add_attribute(jinja: &mut Environment<'_>, ctx: &Arc<EngineCtx>, name: &'static str, code: u8) {
    let on = ctx.colour();
    jinja.add_filter(name, move |value: MjValue| {
        styled(name, &value, |s| Ok(attribute(s, code, on)))
    });
}

pub(super) fn add_hyperlink(jinja: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    let on = ctx.colour();
    jinja.add_function("hyperlink", move |url: MjValue, label: MjValue| {
        hyperlink(&url, &label, on)
    });
}

/// A colour argument: `none` (or undefined) is no colour; a string is a name or
/// `#rrggbb`; an integer 0-255 is a palette index.
fn colour_arg(name: &str, value: &MjValue) -> Result<Option<Colour>, Error> {
    if is_absent(value) {
        return Ok(None);
    }
    let parsed = match value.kind() {
        ValueKind::String => Colour::parse(value.as_str().unwrap_or_default()).ok(),
        ValueKind::Number => value.as_i64().and_then(|n| Colour::index(n).ok()),
        _ => None,
    };
    let shown = match value.as_str() {
        Some(s) => format!("{s:?}"),
        None => value.to_string(),
    };
    parsed.map(Some).ok_or_else(|| {
        op_error(format!(
            "{name}(): unknown colour {shown}: use a name (red, bright_red), 0-255 or #rrggbb"
        ))
    })
}

/// `text` in `colour` on `ground`, closed with a reset; the text itself with
/// no colour, empty text or colour off.
fn paint(text: &str, colour: Option<Colour>, ground: Ground, depth: Depth, on: bool) -> String {
    match colour {
        Some(c) if on && !text.is_empty() => sgr(&c.params(ground, depth), text),
        _ => text.to_string(),
    }
}

/// `text` in SGR attribute `code`, closed with a full reset (`0`) rather than
/// the attribute's own off-code, like the colours: every styling filter ends
/// its segment at a known default state, which is what makes them safe to
/// concatenate in any order.
fn attribute(text: String, code: u8, on: bool) -> String {
    if text.is_empty() || !on {
        return text;
    }
    sgr(&code.to_string(), &text)
}

/// `hyperlink(url, text)`: `text` as an OSC 8 link to `url`. Absent text is
/// undefined, an absent or empty `url` leaves the text unlinked, and a `url`
/// that is not a string, or holds a control character, is an error.
fn hyperlink(url: &MjValue, label: &MjValue, on: bool) -> Result<MjValue, Error> {
    let url = if is_absent(url) {
        None
    } else {
        let u = url
            .as_str()
            .ok_or_else(|| op_error("hyperlink(): the url must be a string"))?;
        if u.chars().any(char::is_control) {
            return Err(op_error("hyperlink(): the url holds a control character"));
        }
        Some(u)
    };
    if is_absent(label) {
        return Ok(UNDEFINED);
    }
    let label = text("hyperlink", label)?;
    Ok(MjValue::from(match url {
        Some(u) if on && !u.is_empty() && !label.is_empty() => {
            format!("\x1b]8;;{u}\x1b\\{label}\x1b]8;;\x1b\\")
        }
        _ => label,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::build_pure_engine_env;

    /// `expr` rendered with `vars` as the render's environment.
    fn eval(vars: &[(&str, &str)], expr: &str) -> Result<String, String> {
        let ctx = EngineCtx {
            env: vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        };
        let jinja = build_pure_engine_env(Arc::new(ctx));
        jinja
            .compile_expression(expr)
            .and_then(|e| e.eval(MjValue::UNDEFINED))
            .map(|v| v.to_string())
            .map_err(|e| e.to_string())
    }

    const TRUE: &[(&str, &str)] = &[("COLORTERM", "truecolor")];

    #[test]
    fn fg_and_bg_wrap_with_correct_sgr() {
        assert_eq!(
            eval(TRUE, "'x' | fg('#ff0080')").unwrap(),
            "\x1b[38;2;255;0;128mx\x1b[0m"
        );
        assert_eq!(
            eval(TRUE, "'x' | bg('#ff0080')").unwrap(),
            "\x1b[48;2;255;0;128mx\x1b[0m"
        );
        assert_eq!(
            eval(TRUE, "'x' | fg('#FF0080')").unwrap(),
            "\x1b[38;2;255;0;128mx\x1b[0m"
        );
    }

    #[test]
    fn rrggbb_renders_the_same_bytes_as_before() {
        assert_eq!(
            eval(TRUE, r##""a" | fg("#a6e22e") | bg("#3e3d32") | bold"##).unwrap(),
            "\x1b[1m\x1b[48;2;62;61;50m\x1b[38;2;166;226;46ma\x1b[0m\x1b[0m\x1b[0m"
        );
    }

    #[test]
    fn colours_by_name_index_and_depth() {
        assert_eq!(eval(&[], "'x' | fg('red')").unwrap(), "\x1b[31mx\x1b[0m");
        assert_eq!(
            eval(&[], "'x' | bg('bright_blue')").unwrap(),
            "\x1b[104mx\x1b[0m"
        );
        assert_eq!(
            eval(&[("TERM", "xterm-256color")], "'x' | bg(236)").unwrap(),
            "\x1b[48;5;236mx\x1b[0m"
        );
        // Without COLORTERM or a 256 TERM the render is 16-colour.
        assert_eq!(
            eval(&[], "'x' | fg('#ff0000')").unwrap(),
            "\x1b[91mx\x1b[0m"
        );
        assert_eq!(
            eval(&[("TERM", "xterm-256color")], "'x' | fg('#ff0000')").unwrap(),
            "\x1b[38;5;196mx\x1b[0m"
        );
        assert_eq!(eval(&[], "'x' | fg(236)").unwrap(), "\x1b[30mx\x1b[0m");
    }

    #[test]
    fn bare_hex_is_an_unknown_colour() {
        let err = eval(TRUE, "'x' | fg('ff0080')").unwrap_err();
        assert!(err.contains("fg(): unknown colour \"ff0080\""), "{err}");
        for bad in [
            "'#fff'",
            "'#ff00801'",
            "'rgb(1,2,3)'",
            "'Red'",
            "256",
            "-1",
            "1.5",
            "true",
            "[1]",
        ] {
            assert!(eval(TRUE, &format!("'x' | bg({bad})")).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_bad_colour_errs_on_empty_and_absent_text() {
        for subject in ["''", "none", "missing"] {
            let err = eval(TRUE, &format!("{subject} | fg('nonsense')")).unwrap_err();
            assert!(err.contains("fg(): unknown colour \"nonsense\""), "{err}");
        }
        // With colour off the colour is still checked.
        assert!(eval(&[("NO_COLOR", "1")], "'x' | fg('nope')").is_err());
    }

    #[test]
    fn empty_and_absent_text_emit_nothing() {
        assert_eq!(eval(TRUE, "'' | fg('red')").unwrap(), "");
        assert_eq!(eval(TRUE, "'' | bold").unwrap(), "");
        assert_eq!(
            eval(TRUE, "(none | fg('red')) is undefined").unwrap(),
            "true"
        );
        assert_eq!(eval(TRUE, "(missing | bold) is undefined").unwrap(), "true");
    }

    #[test]
    fn none_applies_no_colour() {
        assert_eq!(eval(TRUE, "'x' | fg(none)").unwrap(), "x");
        assert_eq!(eval(TRUE, "'x' | bg(missing)").unwrap(), "x");
        assert_eq!(
            eval(TRUE, "'x' | fg(none) | bold").unwrap(),
            "\x1b[1mx\x1b[0m"
        );
    }

    #[test]
    fn italic_underline_invert() {
        for (filter, code) in [
            ("italic", 3),
            ("underline", 4),
            ("invert", 7),
            ("bold", 1),
            ("dim", 2),
        ] {
            assert_eq!(
                eval(&[], &format!("'x' | {filter}")).unwrap(),
                format!("\x1b[{code}mx\x1b[0m")
            );
            assert_eq!(eval(&[], &format!("'' | {filter}")).unwrap(), "");
            assert_eq!(
                eval(&[("NO_COLOR", "1")], &format!("'x' | {filter}")).unwrap(),
                "x"
            );
        }
    }

    #[test]
    fn colour_off_hands_the_text_back() {
        let off = &[("NO_COLOR", "1"), ("COLORTERM", "truecolor")];
        assert_eq!(eval(off, "'x' | fg('#ff0080')").unwrap(), "x");
        assert_eq!(eval(off, "'x' | bg(3) | bold").unwrap(), "x");
        // An empty NO_COLOR is not set.
        assert_eq!(
            eval(&[("NO_COLOR", "")], "'x' | fg('red')").unwrap(),
            "\x1b[31mx\x1b[0m"
        );
    }

    #[test]
    fn filters_compose_and_nest() {
        assert_eq!(
            eval(TRUE, r##"("a" | fg("#ff0000")) ~ ("b" | fg("#00ff00"))"##).unwrap(),
            "\x1b[38;2;255;0;0ma\x1b[0m\x1b[38;2;0;255;0mb\x1b[0m"
        );
        assert_eq!(
            eval(TRUE, r##""x" | fg("#ff0000") | bold"##).unwrap(),
            "\x1b[1m\x1b[38;2;255;0;0mx\x1b[0m\x1b[0m"
        );
        assert_eq!(
            eval(TRUE, r##""x" | fg("#ff0000") | bg("#0000ff")"##).unwrap(),
            "\x1b[48;2;0;0;255m\x1b[38;2;255;0;0mx\x1b[0m\x1b[0m"
        );
    }

    #[test]
    fn hyperlink_wraps_text_in_osc_8() {
        assert_eq!(
            eval(&[], "hyperlink('https://x.test/1', '#1')").unwrap(),
            "\x1b]8;;https://x.test/1\x1b\\#1\x1b]8;;\x1b\\"
        );
        assert_eq!(
            eval(&[], "hyperlink('u', 7)").unwrap(),
            "\x1b]8;;u\x1b\\7\x1b]8;;\x1b\\"
        );
    }

    #[test]
    fn hyperlink_absent_text_is_undefined_and_absent_url_is_the_text() {
        for text in ["none", "missing"] {
            assert_eq!(
                eval(&[], &format!("hyperlink('u', {text}) is undefined")).unwrap(),
                "true"
            );
        }
        assert_eq!(eval(&[], "hyperlink(none, 'text')").unwrap(), "text");
        assert_eq!(eval(&[], "hyperlink(missing, 'text')").unwrap(), "text");
        assert_eq!(eval(&[], "hyperlink('u', '')").unwrap(), "");
    }

    #[test]
    fn hyperlink_errors_and_colour_off() {
        let err = eval(&[], "hyperlink(3, 'x')").unwrap_err();
        assert!(
            err.contains("hyperlink(): the url must be a string"),
            "{err}"
        );
        let err = eval(&[], "hyperlink('a\\u001b[0m', 'x')").unwrap_err();
        assert!(
            err.contains("hyperlink(): the url holds a control character"),
            "{err}"
        );
        // A bad url is an error even with no text.
        assert!(eval(&[], "hyperlink(3, none)").is_err());
        assert_eq!(
            eval(&[("NO_COLOR", "1")], "hyperlink('u', 'text')").unwrap(),
            "text"
        );
    }
}
