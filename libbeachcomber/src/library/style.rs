//! Text and terminal styling: `truncate`, `fg`, `bg`, `bold` and `dim`.
//!
//! All are pure string transforms (no filesystem, network or process access),
//! so unlike `exec()` they need no `allow_exec` gating and are on every
//! surface. The colour filters draw escapes only when the render's `env` does
//! not set `NO_COLOR` ([`EngineCtx::colour`]).

use crate::engine::EngineCtx;
use minijinja::{Environment, Error, ErrorKind};
use std::sync::Arc;

pub(super) fn add_truncate(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("truncate", truncate_filter);
}

pub(super) fn add_fg(env: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    let colour = ctx.colour();
    env.add_filter("fg", move |value: String, color: String| {
        fg_filter(value, color, colour)
    });
}

pub(super) fn add_bg(env: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    let colour = ctx.colour();
    env.add_filter("bg", move |value: String, color: String| {
        bg_filter(value, color, colour)
    });
}

pub(super) fn add_bold(env: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    let colour = ctx.colour();
    env.add_filter("bold", move |value: String| bold_filter(value, colour));
}

pub(super) fn add_dim(env: &mut Environment<'_>, ctx: &Arc<EngineCtx>) {
    let colour = ctx.colour();
    env.add_filter("dim", move |value: String| dim_filter(value, colour));
}

fn truncate_filter(value: String, length: u32) -> String {
    if value.chars().count() <= length as usize {
        value
    } else {
        let mut s: String = value.chars().take(length as usize).collect();
        s.push_str("...");
        s
    }
}

// ── fg/bg: hex colour → truecolor SGR ──────────────────────────────────────

/// `{{ text | fg("#rrggbb") }}` — wrap `text` in a 24-bit foreground SGR
/// escape (`\x1b[38;2;R;G;Bm` ... `\x1b[0m`).
///
/// Comb values store colours as `#rrggbb` hex (e.g. scanline's published
/// theme, config-file palettes) because hex is the one canonical
/// representation every consumer can parse. A consumer rendering to a
/// terminal — a shell prompt, a statusline template, `comb eval`
/// piped into a prompt string — needs SGR instead, so the conversion
/// happens here, at the point of use, rather than by storing colours in
/// two formats.
///
/// Empty input text returns empty string with no escapes emitted at all,
/// so a conditional colour segment composes cleanly: `{{ x | fg(c) }}`
/// contributes nothing when `x` is empty rather than an orphan pair of
/// escapes around nothing.
///
/// An unparseable colour is a filter error (`ErrorKind::InvalidOperation`,
/// naming the offending value), not silent passthrough — a typo'd colour
/// must be visible rather than rendering the text uncoloured with no sign
/// anything went wrong.
fn fg_filter(value: String, color: String, colour: bool) -> Result<String, Error> {
    sgr_wrap(value, &color, 38, colour)
}

/// `{{ text | bg("#rrggbb") }}` — [`fg_filter`]'s sibling for the
/// background SGR parameter (`48;2` instead of `38;2`). Same colour
/// parsing, same empty-input and error behaviour.
fn bg_filter(value: String, color: String, colour: bool) -> Result<String, Error> {
    sgr_wrap(value, &color, 48, colour)
}

/// Shared body of `fg`/`bg`: parse `color` and wrap non-empty `value` in
/// the SGR escape pair for `sgr_code` (38 for foreground, 48 for
/// background).
fn sgr_wrap(value: String, color: &str, sgr_code: u8, colour: bool) -> Result<String, Error> {
    if value.is_empty() {
        return Ok(String::new());
    }
    let (r, g, b) = parse_hex_color(color)?;
    if !colour {
        return Ok(value);
    }
    Ok(format!("\x1b[{sgr_code};2;{r};{g};{b}m{value}\x1b[0m"))
}

// ── bold/dim: SGR attributes ───────────────────────────────────────────────

/// `{{ text | bold }}` — wrap `text` in the bold SGR pair
/// (`\x1b[1m` ... `\x1b[0m`).
///
/// The attribute counterpart of [`fg_filter`]/[`bg_filter`]: a terminal
/// consumer needs bold and dim as often as it needs colour, and neither is
/// expressible as a colour argument. Kept as its own filter rather than a
/// keyword on `fg` so bold text with no colour of its own — a session name,
/// a heading — needs no placeholder colour, and so the four compose in any
/// order: `{{ x | fg(c) | bg(d) | bold }}` nests to one cell style.
///
/// Empty input returns empty with no escapes at all, same as `fg`/`bg`, so
/// a conditional segment contributes nothing rather than leaving a stray
/// attribute escape that would bleed into whatever follows it.
fn bold_filter(value: String, colour: bool) -> String {
    sgr_attr(value, 1, colour)
}

/// `{{ text | dim }}` — [`bold_filter`]'s sibling for the dim/faint SGR
/// attribute (`2` instead of `1`). Same empty-input behaviour.
fn dim_filter(value: String, colour: bool) -> String {
    sgr_attr(value, 2, colour)
}

/// Shared body of `bold`/`dim`: wrap non-empty `value` in the SGR pair for
/// `code`. The closer is a full reset (`0`) rather than the attribute's own
/// off-code (`22`), matching `fg`/`bg` — every one of these filters ends its
/// segment by returning the terminal to a known default state, which is what
/// makes them safe to concatenate in any order.
fn sgr_attr(value: String, code: u8, colour: bool) -> String {
    if value.is_empty() || !colour {
        return value;
    }
    format!("\x1b[{code}m{value}\x1b[0m")
}

/// Parse a `#rrggbb` or bare `rrggbb` colour (case-insensitive — a stored
/// value may arrive already stripped of its `#`) into its RGB bytes.
///
/// Anything else — wrong length, non-hex digits, `rgb()`/named colours —
/// is an `InvalidOperation` error naming the value that failed to parse,
/// so a typo surfaces at render time instead of silently passing the text
/// through uncoloured.
fn parse_hex_color(color: &str) -> Result<(u8, u8, u8), Error> {
    let hex = color.strip_prefix('#').unwrap_or(color);
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(Error::new(
            ErrorKind::InvalidOperation,
            format!("invalid colour {color:?}: expected #rrggbb hex"),
        ));
    }
    let r = u8::from_str_radix(&hex[0..2], 16).expect("validated hex digits");
    let g = u8::from_str_radix(&hex[2..4], 16).expect("validated hex digits");
    let b = u8::from_str_radix(&hex[4..6], 16).expect("validated hex digits");
    Ok((r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::build_pure_engine_env;

    #[test]
    fn fg_wraps_with_correct_sgr() {
        assert_eq!(
            fg_filter("x".to_string(), "#ff0080".to_string(), true).unwrap(),
            "\x1b[38;2;255;0;128mx\x1b[0m"
        );
    }

    #[test]
    fn bg_wraps_with_correct_sgr() {
        assert_eq!(
            bg_filter("x".to_string(), "#ff0080".to_string(), true).unwrap(),
            "\x1b[48;2;255;0;128mx\x1b[0m"
        );
    }

    #[test]
    fn hex_parsing_is_case_insensitive() {
        assert_eq!(
            fg_filter("x".to_string(), "#FF0080".to_string(), true).unwrap(),
            "\x1b[38;2;255;0;128mx\x1b[0m"
        );
    }

    #[test]
    fn hash_prefix_is_optional() {
        assert_eq!(
            fg_filter("x".to_string(), "ff0080".to_string(), true).unwrap(),
            "\x1b[38;2;255;0;128mx\x1b[0m"
        );
    }

    #[test]
    fn invalid_colour_errors_naming_the_value() {
        let err = fg_filter("x".to_string(), "not-a-colour".to_string(), true).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidOperation);
        assert!(
            err.to_string().contains("not-a-colour"),
            "error should name the offending value, got: {err}"
        );
    }

    #[test]
    fn wrong_length_hex_errors() {
        assert!(fg_filter("x".to_string(), "#fff".to_string(), true).is_err());
        assert!(fg_filter("x".to_string(), "#ff00801".to_string(), true).is_err());
    }

    #[test]
    fn empty_text_emits_no_escapes() {
        assert_eq!(
            fg_filter(String::new(), "#ff0080".to_string(), true).unwrap(),
            ""
        );
        assert_eq!(
            bg_filter(String::new(), "#ff0080".to_string(), true).unwrap(),
            ""
        );
    }

    #[test]
    fn empty_text_with_invalid_colour_still_short_circuits() {
        // Empty-input short-circuit happens before colour parsing, so a
        // template that conditionally colours empty text never trips over
        // an otherwise-invalid placeholder colour.
        assert_eq!(
            fg_filter(String::new(), "nonsense".to_string(), true).unwrap(),
            ""
        );
    }

    #[test]
    fn filters_compose_via_concatenation() {
        let env = build_pure_engine_env(Default::default());
        let expr = env
            .compile_expression(r##"("a" | fg("#ff0000")) ~ ("b" | fg("#00ff00"))"##)
            .unwrap();
        let out = expr.eval(minijinja::Value::UNDEFINED).unwrap();
        assert_eq!(
            out.as_str().unwrap(),
            "\x1b[38;2;255;0;0ma\x1b[0m\x1b[38;2;0;255;0mb\x1b[0m"
        );
    }

    #[test]
    fn bold_and_dim_wrap_with_their_sgr_attribute() {
        assert_eq!(bold_filter("x".to_string(), true), "\x1b[1mx\x1b[0m");
        assert_eq!(dim_filter("x".to_string(), true), "\x1b[2mx\x1b[0m");
    }

    #[test]
    fn bold_and_dim_empty_text_emits_no_escapes() {
        assert_eq!(bold_filter(String::new(), true), "");
        assert_eq!(dim_filter(String::new(), true), "");
    }

    #[test]
    fn colour_off_hands_the_text_back_and_still_checks_the_colour() {
        assert_eq!(
            fg_filter("x".to_string(), "#ff0080".to_string(), false).unwrap(),
            "x"
        );
        assert_eq!(bold_filter("x".to_string(), false), "x");
        assert!(fg_filter("x".to_string(), "nope".to_string(), false).is_err());
    }

    #[test]
    fn bold_composes_over_fg() {
        let env = build_pure_engine_env(Default::default());
        let expr = env
            .compile_expression(r##""x" | fg("#ff0000") | bold"##)
            .unwrap();
        let out = expr.eval(minijinja::Value::UNDEFINED).unwrap();
        assert_eq!(
            out.as_str().unwrap(),
            "\x1b[1m\x1b[38;2;255;0;0mx\x1b[0m\x1b[0m"
        );
    }

    #[test]
    fn fg_nests_inside_bg() {
        let env = build_pure_engine_env(Default::default());
        let expr = env
            .compile_expression(r##""x" | fg("#ff0000") | bg("#0000ff")"##)
            .unwrap();
        let out = expr.eval(minijinja::Value::UNDEFINED).unwrap();
        // bg wraps the fg-wrapped string as its input text: escapes nest.
        assert_eq!(
            out.as_str().unwrap(),
            "\x1b[48;2;0;0;255m\x1b[38;2;255;0;0mx\x1b[0m\x1b[0m"
        );
    }
}
