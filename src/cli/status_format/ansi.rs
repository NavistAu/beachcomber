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

/// Whether the terminal's background is light, from `COLORFGBG` ("fg;bg",
/// sometimes "fg;default;bg" — some terminals export it, none guarantee it):
/// a background index of 0–6 or 8 is dark. Unset or unparsable is dark, the
/// common case.
pub fn light_background() -> bool {
    std::env::var("COLORFGBG").ok().is_some_and(|v| {
        v.rsplit(';')
            .next()
            .and_then(|bg| bg.parse::<u8>().ok())
            .is_some_and(|bg| !(bg <= 6 || bg == 8))
    })
}

/// Subtle background for alternating data rows: one step off the terminal's
/// own background on the 256-colour grayscale ramp ([`light_background`]).
/// Killed entirely by --color=never / NO_COLOR / non-TTY.
pub fn stripe_bg() -> &'static str {
    const DARK: &str = "\x1b[48;5;234m"; // faint lift off black
    const LIGHT: &str = "\x1b[48;5;254m"; // faint dip off white
    if light_background() { LIGHT } else { DARK }
}

/// `s` dimmed when colour is on, as a shadowed tree row is.
pub fn dim(s: &str, color_enabled: bool) -> String {
    wrap(s, Color::Dim, color_enabled)
}

/// `s` in bold when colour is on, as a table's header is.
pub fn bold(s: &str, color_enabled: bool) -> String {
    wrap(s, Color::Bold, color_enabled)
}

/// A log level's name coloured, when colour is on, by a 256-colour index
/// chosen for at least 7:1 contrast (WCAG AAA) against the background
/// [`light_background`] assumes: black for a dark terminal, white for a light
/// one. `debug` is a grey that keeps 7:1, where SGR dim would not.
pub fn level(level: libbeachcomber::protocol::Level, text: &str, enabled: bool) -> String {
    if !enabled {
        return text.to_string();
    }
    term::sgr(&level_params(level, light_background()), text)
}

/// [`level`]'s SGR parameters, dark or `light`.
fn level_params(level: libbeachcomber::protocol::Level, light: bool) -> String {
    use libbeachcomber::protocol::Level;
    let index = match (level, light) {
        // #ff8787: 9.1:1 on black, 7.4:1 on #1c1c1c.
        (Level::Error | Level::Fatal, false) => "210",
        // #af0000: 7.4:1 on white.
        (Level::Error | Level::Fatal, true) => "124",
        // #ffff87: 19.9:1.
        (Level::Warn, false) => "228",
        // #870087: 8.8:1.
        (Level::Warn, true) => "90",
        // #87d7ff: 13.2:1.
        (Level::Info, false) => "117",
        // #000087: 15.7:1.
        (Level::Info, true) => "18",
        // #a8a8a8: 8.8:1, 7.2:1 on #1c1c1c.
        (Level::Debug, false) => "248",
        // #585858: 7.1:1.
        (Level::Debug, true) => "240",
    };
    let bold = if level == Level::Fatal { "1;" } else { "" };
    format!("{bold}38;5;{index}")
}

#[cfg(test)]
mod ansi_tests {
    use super::*;

    #[test]
    fn a_level_keeps_its_contrast_and_fatal_is_bold() {
        use libbeachcomber::protocol::Level;
        assert_eq!(level_params(Level::Warn, false), "38;5;228");
        assert_eq!(level_params(Level::Warn, true), "38;5;90");
        assert_eq!(level_params(Level::Fatal, false), "1;38;5;210");
        assert_eq!(level(Level::Warn, "warn", false), "warn");
    }

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
