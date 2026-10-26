//! How comb's own output looks, decided once per run: whether it is
//! coloured, and which glyphs it draws with.
//!
//! The global `--color` wins over `NO_COLOR`, which counts only when it is
//! set to something (an empty `NO_COLOR` is unset, as no-color.org has it).
//! `main` writes the flag into the environment before anything runs, so the
//! template filters (`fg`, `bg`, `bold`, `dim`), which read `NO_COLOR` from
//! the render's environment, and every `exec()` child see the same choice.
//! A table is coloured only on a terminal unless `--color always` says so; a
//! template filter is not, since a prompt reads it through a pipe.

use std::ffi::OsStr;
use std::io::IsTerminal;

/// `--color`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum ColorChoice {
    /// Colour on a terminal, unless `NO_COLOR` is set
    #[default]
    Auto,
    /// Colour, whatever `NO_COLOR` says
    Always,
    /// No colour
    Never,
}

impl ColorChoice {
    /// Settle `NO_COLOR` in the process environment to match the flag, so
    /// everything that reads it agrees. Call before any thread starts.
    pub fn apply_to_env(self) {
        match self {
            ColorChoice::Auto => {}
            // SAFETY: `main` calls this first, while the process has one thread.
            ColorChoice::Always => unsafe { std::env::remove_var("NO_COLOR") },
            ColorChoice::Never => unsafe { std::env::set_var("NO_COLOR", "1") },
        }
    }
}

/// Whether a `NO_COLOR` value turns colour off: any value but the empty one.
pub fn no_color(value: Option<&OsStr>) -> bool {
    value.is_some_and(|v| !v.is_empty())
}

/// The glyphs a table, tree or legend is drawn with.
#[derive(Debug, PartialEq, Eq)]
pub struct Glyphs {
    /// An Active entry.
    pub star: &'static str,
    /// A failing entry.
    pub warn: &'static str,
    /// AGE × polls.
    pub times: &'static str,
    /// An entry that watches its files while it is Active.
    pub dot: &'static str,
    /// A value rendered here.
    pub rendered: &'static str,
    /// A value whose text was sanitised.
    pub tainted: &'static str,
    /// A tree branch with more below it, and the last one.
    pub branch: &'static str,
    pub last_branch: &'static str,
    pub ellipsis: &'static str,
    /// The dash of a sentence, and of a cell with nothing in it.
    pub dash: &'static str,
}

pub const UNICODE: Glyphs = Glyphs {
    star: "\u{2605}",
    warn: "\u{26a0}",
    times: "\u{00d7}",
    dot: "\u{2219}",
    rendered: "\u{25b8}",
    tainted: "\u{203c}",
    branch: "\u{251c}\u{2500}",
    last_branch: "\u{2514}\u{2500}",
    ellipsis: "\u{2026}",
    dash: "\u{2014}",
};

pub const ASCII: Glyphs = Glyphs {
    star: "*",
    warn: "!",
    times: "x",
    dot: "-",
    rendered: ">",
    tainted: "!!",
    branch: "|-",
    last_branch: "`-",
    ellipsis: "...",
    dash: "-",
};

impl Glyphs {
    pub fn of(ascii: bool) -> &'static Glyphs {
        if ascii { &ASCII } else { &UNICODE }
    }
}

/// Colour and glyphs for one run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    pub color: bool,
    pub ascii: bool,
}

impl Style {
    /// `choice` decides, then `NO_COLOR`, then whether stdout is a terminal.
    ///
    /// `auto` deliberately has no environment-based escape hatch for pipes. An
    /// earlier revision promoted `auto` to colour when `WATCH_INTERVAL` was set,
    /// on the belief that `watch(1)` exports it. It does not — procps-ng *reads*
    /// `WATCH_INTERVAL` to pick its own default interval and never passes it to
    /// the child. The promotion therefore never fired under `watch`, and when a
    /// user did export the variable for its documented purpose it fired at the
    /// worst time: plain `watch` (no `-c`) does not interpret SGR, so the table
    /// filled with literal escape text. Live viewers should ask for colour
    /// explicitly: `watch -c 'comb status --color=always'`.
    pub fn new(choice: ColorChoice, no_color: bool, is_tty: bool, ascii: bool) -> Style {
        let color = match choice {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => !no_color && is_tty,
        };
        Style { color, ascii }
    }

    /// The style of this run, from its flags and its environment.
    pub fn detect(choice: ColorChoice, ascii: bool) -> Style {
        Style::new(
            choice,
            no_color(std::env::var_os("NO_COLOR").as_deref()),
            std::io::stdout().is_terminal(),
            ascii,
        )
    }

    pub fn glyphs(self) -> &'static Glyphs {
        Glyphs::of(self.ascii)
    }

    /// `text` with its dashes drawn in this style: `check` relays the
    /// daemon's own sentences beside its own, so they are redrawn here, at
    /// the edge, rather than each where it is written.
    pub fn dashes(self, text: &str) -> std::borrow::Cow<'_, str> {
        if self.ascii {
            text.replace(UNICODE.dash, ASCII.dash).into()
        } else {
            text.into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flag_decides_then_no_color_then_the_terminal() {
        let color = |choice, no_color, tty| Style::new(choice, no_color, tty, false).color;
        assert!(color(ColorChoice::Always, true, false));
        assert!(!color(ColorChoice::Never, false, true));
        assert!(color(ColorChoice::Auto, false, true));
        assert!(!color(ColorChoice::Auto, true, true));
        // A pipe with no explicit --color: no colour. There is no
        // environment escape hatch — `watch(1)` never exported
        // WATCH_INTERVAL, it reads it.
        assert!(!color(ColorChoice::Auto, false, false));
    }

    #[test]
    fn an_empty_no_color_is_unset() {
        assert!(!no_color(None));
        assert!(!no_color(Some(OsStr::new(""))));
        assert!(no_color(Some(OsStr::new("1"))));
        assert!(no_color(Some(OsStr::new("0"))));
    }
}
