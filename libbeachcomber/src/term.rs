//! Terminal colour: what a colour is, how a render's `env` decides how deep the
//! terminal can show it, and the SGR escape for it.
//!
//! A colour is a name (the 16 ANSI colours, `bright_` for the upper eight), an
//! index in the 256-colour palette, or `#rrggbb`. The depth comes from the
//! environment the render was given: `COLORTERM` of `truecolor` or `24bit` is
//! 24-bit, else a `TERM` containing `256color` is 256, else 16. A colour deeper
//! than the terminal is mapped to the nearest one it has.

use std::collections::HashMap;

/// How many colours a terminal shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Depth {
    /// The 16 ANSI colours.
    Basic,
    /// The 256-colour palette.
    Palette,
    /// 24-bit colour.
    TrueColor,
}

impl Depth {
    /// The depth `env` (a render's whole environment) asks for.
    pub fn of(env: &HashMap<String, String>) -> Depth {
        let get = |k: &str| env.get(k).map(String::as_str).unwrap_or("");
        if matches!(get("COLORTERM"), "truecolor" | "24bit") {
            Depth::TrueColor
        } else if get("TERM").contains("256color") {
            Depth::Palette
        } else {
            Depth::Basic
        }
    }
}

/// Whether a render's `env` allows colour: unless `NO_COLOR` is set to
/// something.
pub fn colour_on(env: &HashMap<String, String>) -> bool {
    env.get("NO_COLOR").is_none_or(|v| v.is_empty())
}

/// Foreground or background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ground {
    Fore,
    Back,
}

/// A colour, as written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colour {
    /// One of the 16 ANSI colours, 0-15.
    Ansi(u8),
    /// An index in the 256-colour palette.
    Index(u8),
    /// 24-bit.
    Rgb(u8, u8, u8),
}

const NAMES: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

/// Why a colour did not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColourError;

impl Colour {
    /// A colour from text: a name, `bright_` and a name, or `#rrggbb`. Nothing
    /// else parses: not a bare `rrggbb`, not `#rgb`, not `rgb()`.
    pub fn parse(text: &str) -> Result<Colour, ColourError> {
        if let Some(hex) = text.strip_prefix('#') {
            if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(ColourError);
            }
            let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| ColourError);
            return Ok(Colour::Rgb(byte(0)?, byte(2)?, byte(4)?));
        }
        let (bright, name) = match text.strip_prefix("bright_") {
            Some(name) => (8, name),
            None => (0, text),
        };
        NAMES
            .iter()
            .position(|n| *n == name)
            .map(|i| Colour::Ansi(bright + i as u8))
            .ok_or(ColourError)
    }

    /// A palette index, 0-255.
    pub fn index(n: i64) -> Result<Colour, ColourError> {
        u8::try_from(n).map(Colour::Index).map_err(|_| ColourError)
    }

    /// The SGR parameters that set this colour on `ground` at `depth`.
    pub fn params(self, ground: Ground, depth: Depth) -> String {
        let (base, bright_base, extended) = match ground {
            Ground::Fore => (30, 90, 38),
            Ground::Back => (40, 100, 48),
        };
        let ansi = |n: u8| {
            if n < 8 {
                (base + n).to_string()
            } else {
                (bright_base + n - 8).to_string()
            }
        };
        match self {
            Colour::Ansi(n) => ansi(n),
            Colour::Index(n) if depth >= Depth::Palette => format!("{extended};5;{n}"),
            Colour::Index(n) => {
                let (r, g, b) = palette_rgb(n);
                ansi(nearest(r, g, b, 0..16))
            }
            Colour::Rgb(r, g, b) => match depth {
                Depth::TrueColor => format!("{extended};2;{r};{g};{b}"),
                Depth::Palette => format!("{extended};5;{}", nearest(r, g, b, 16..256)),
                Depth::Basic => ansi(nearest(r, g, b, 0..16)),
            },
        }
    }
}

/// The RGB xterm shows for palette index `n`.
fn palette_rgb(n: u8) -> (u8, u8, u8) {
    const BASE: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 0, 0),
        (0, 205, 0),
        (205, 205, 0),
        (0, 0, 238),
        (205, 0, 205),
        (0, 205, 205),
        (229, 229, 229),
        (127, 127, 127),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (92, 92, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match n {
        0..=15 => BASE[n as usize],
        16..=231 => {
            let i = n - 16;
            (
                LEVELS[(i / 36) as usize],
                LEVELS[((i / 6) % 6) as usize],
                LEVELS[(i % 6) as usize],
            )
        }
        232..=255 => {
            let v = 8 + 10 * (n - 232);
            (v, v, v)
        }
    }
}

/// The palette index in `range` nearest `(r, g, b)` by squared RGB distance,
/// the lowest on a tie.
fn nearest(r: u8, g: u8, b: u8, range: std::ops::Range<u16>) -> u8 {
    let distance = |n: u16| {
        let (pr, pg, pb) = palette_rgb(n as u8);
        let d = |a: u8, b: u8| (i32::from(a) - i32::from(b)).pow(2);
        d(r, pr) + d(g, pg) + d(b, pb)
    };
    range
        .min_by_key(|n| (distance(*n), *n))
        .expect("a palette range is never empty") as u8
}

/// `text` inside the SGR sequence `params`, closed with a full reset.
pub fn sgr(params: &str, text: &str) -> String {
    format!("\x1b[{params}m{text}\x1b[0m")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn fg(c: Colour, depth: Depth) -> String {
        c.params(Ground::Fore, depth)
    }

    #[test]
    fn colour_names_are_the_sixteen() {
        let names = [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ];
        for (i, name) in names.iter().enumerate() {
            let plain = Colour::parse(name).unwrap();
            let bright = Colour::parse(&format!("bright_{name}")).unwrap();
            assert_eq!(fg(plain, Depth::Basic), (30 + i).to_string());
            assert_eq!(fg(bright, Depth::Basic), (90 + i).to_string());
            assert_eq!(
                plain.params(Ground::Back, Depth::Basic),
                (40 + i).to_string()
            );
            assert_eq!(
                bright.params(Ground::Back, Depth::TrueColor),
                (100 + i).to_string(),
                "a name is the same at every depth"
            );
        }
    }

    #[test]
    fn colour_text_that_is_not_a_name_or_full_hex_is_refused() {
        for bad in [
            "",
            "ff0080",
            "#fff",
            "#ff00801",
            "#gg0000",
            "rgb(1,2,3)",
            "Red",
            "bright_",
            "bright",
            "grey",
            "bright_bright_red",
            "7",
        ] {
            assert!(Colour::parse(bad).is_err(), "{bad:?}");
        }
        assert_eq!(Colour::parse("#FF0080"), Ok(Colour::Rgb(255, 0, 128)));
    }

    #[test]
    fn colour_index_is_the_palette() {
        assert_eq!(Colour::index(0), Ok(Colour::Index(0)));
        assert_eq!(Colour::index(255), Ok(Colour::Index(255)));
        assert!(Colour::index(256).is_err());
        assert!(Colour::index(-1).is_err());
        assert_eq!(fg(Colour::Index(236), Depth::Palette), "38;5;236");
        assert_eq!(
            Colour::Index(236).params(Ground::Back, Depth::TrueColor),
            "48;5;236"
        );
    }

    #[test]
    fn colour_index_at_sixteen_maps_to_the_nearest_ansi() {
        // The first sixteen are exact; the rest map by distance.
        assert_eq!(fg(Colour::Index(1), Depth::Basic), "31");
        assert_eq!(fg(Colour::Index(9), Depth::Basic), "91");
        assert_eq!(fg(Colour::Index(196), Depth::Basic), "91");
        assert_eq!(fg(Colour::Index(232), Depth::Basic), "30");
        assert_eq!(fg(Colour::Index(255), Depth::Basic), "37");
    }

    #[test]
    fn colour_hex_at_24_bit_is_truecolor() {
        assert_eq!(
            fg(Colour::Rgb(255, 0, 128), Depth::TrueColor),
            "38;2;255;0;128"
        );
        assert_eq!(
            Colour::Rgb(0, 0, 255).params(Ground::Back, Depth::TrueColor),
            "48;2;0;0;255"
        );
    }

    #[test]
    fn colour_hex_at_256_is_the_nearest_cube_or_grey() {
        assert_eq!(fg(Colour::Rgb(255, 0, 0), Depth::Palette), "38;5;196");
        // Exactly the grey ramp's 128.
        assert_eq!(fg(Colour::Rgb(128, 128, 128), Depth::Palette), "38;5;244");
        assert_eq!(fg(Colour::Rgb(95, 135, 175), Depth::Palette), "38;5;67");
        assert_eq!(
            Colour::Rgb(255, 0, 0).params(Ground::Back, Depth::Palette),
            "48;5;196"
        );
    }

    #[test]
    fn colour_hex_at_16_is_the_nearest_ansi() {
        assert_eq!(fg(Colour::Rgb(255, 0, 0), Depth::Basic), "91");
        assert_eq!(fg(Colour::Rgb(0, 0, 0), Depth::Basic), "30");
        assert_eq!(fg(Colour::Rgb(250, 250, 250), Depth::Basic), "97");
        assert_eq!(fg(Colour::Rgb(0, 0, 230), Depth::Basic), "34");
        assert_eq!(
            Colour::Rgb(205, 0, 0).params(Ground::Back, Depth::Basic),
            "41"
        );
    }

    #[test]
    fn colour_depth_comes_from_colorterm_then_term() {
        assert_eq!(Depth::of(&env(&[])), Depth::Basic);
        assert_eq!(Depth::of(&env(&[("TERM", "xterm")])), Depth::Basic);
        assert_eq!(
            Depth::of(&env(&[("TERM", "xterm-256color")])),
            Depth::Palette
        );
        assert_eq!(
            Depth::of(&env(&[("COLORTERM", "truecolor")])),
            Depth::TrueColor
        );
        assert_eq!(
            Depth::of(&env(&[("COLORTERM", "24bit"), ("TERM", "dumb")])),
            Depth::TrueColor
        );
        assert_eq!(
            Depth::of(&env(&[("COLORTERM", "yes"), ("TERM", "screen-256color")])),
            Depth::Palette
        );
    }

    #[test]
    fn colour_off_is_a_non_empty_no_color() {
        assert!(colour_on(&env(&[])));
        assert!(colour_on(&env(&[("NO_COLOR", "")])));
        assert!(!colour_on(&env(&[("NO_COLOR", "1")])));
    }

    #[test]
    fn colour_sgr_closes_with_a_reset() {
        assert_eq!(sgr("31", "x"), "\x1b[31mx\x1b[0m");
    }
}
