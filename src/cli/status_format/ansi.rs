//! ANSI colour helper (basic-16 palette, fg-only), on `libbeachcomber::term`.

use libbeachcomber::term;

#[derive(Clone, Copy, Debug)]
pub enum Color {
    BrightGreen,
    Yellow,
    BrightYellow,
    Red,
    Cyan,
    Magenta,
    Dim,
    Bold,
}

impl Color {
    /// The SGR parameters: a colour name through [`term::Colour`], or an
    /// attribute.
    pub fn params(self) -> String {
        let named = |name: &str| {
            term::Colour::parse(name)
                .expect("a colour name")
                .params(term::Ground::Fore, term::Depth::Basic)
        };
        match self {
            Color::BrightGreen => named("bright_green"),
            Color::Yellow => named("yellow"),
            Color::BrightYellow => named("bright_yellow"),
            Color::Red => named("red"),
            Color::Cyan => named("cyan"),
            Color::Magenta => named("magenta"),
            Color::Dim => "2".to_string(),
            Color::Bold => "1".to_string(),
        }
    }
}

pub fn wrap(s: &str, c: Color, enabled: bool) -> String {
    if !enabled {
        return s.to_string();
    }
    term::sgr(&c.params(), s)
}

pub const RESET: &str = "\x1b[0m";

/// Subtle background for alternating data rows: one step off the terminal's
/// own background on the 256-colour grayscale ramp. The terminal's polarity
/// comes from `COLORFGBG` ("fg;bg", sometimes "fg;default;bg" — some
/// terminals export it, none guarantee it): a background index of 0–6 or 8
/// is dark. Unset or unparsable defaults to dark, the common case. Killed
/// entirely by --color=never / NO_COLOR / non-TTY.
pub fn stripe_bg() -> &'static str {
    const DARK: &str = "\x1b[48;5;234m"; // faint lift off black
    const LIGHT: &str = "\x1b[48;5;254m"; // faint dip off white
    match std::env::var("COLORFGBG") {
        Ok(v) => match v.rsplit(';').next().and_then(|bg| bg.parse::<u8>().ok()) {
            Some(bg) if bg <= 6 || bg == 8 => DARK,
            Some(_) => LIGHT,
            None => DARK,
        },
        Err(_) => DARK,
    }
}

/// `s` dimmed when colour is on, as a shadowed tree row is.
pub fn dim(s: &str, color_enabled: bool) -> String {
    wrap(s, Color::Dim, color_enabled)
}

/// `s` in bold when colour is on, as a table's header is.
pub fn bold(s: &str, color_enabled: bool) -> String {
    wrap(s, Color::Bold, color_enabled)
}

#[cfg(test)]
mod ansi_tests {
    use super::*;

    #[test]
    fn colour_codes_match_basic_16() {
        assert_eq!(wrap("hi", Color::BrightGreen, true), "\x1b[92mhi\x1b[0m");
        assert_eq!(wrap("hi", Color::Yellow, true), "\x1b[33mhi\x1b[0m");
        assert_eq!(wrap("hi", Color::BrightYellow, true), "\x1b[93mhi\x1b[0m");
        assert_eq!(wrap("hi", Color::Red, true), "\x1b[31mhi\x1b[0m");
        assert_eq!(wrap("hi", Color::Dim, true), "\x1b[2mhi\x1b[0m");
    }

    #[test]
    fn colour_disabled_returns_plain() {
        assert_eq!(wrap("hi", Color::Red, false), "hi");
    }
}
