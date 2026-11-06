//! What comb's exit status says, the same for every command: each command
//! returns an [`Outcome`], and `main` turns it into the process's exit code.
//!
//! - `0` — done. A read that found nothing is done too, in every format: the
//!   format is applied last and never decides the code.
//! - `1` — a miss under `--strict` (`BEACHCOMBER_STRICT`), or `check` found
//!   something worth a look (a `WARN`).
//! - `2` — an error.

use std::process::ExitCode;

/// How a command ended, worst last, so outcomes combine with `max`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Outcome {
    Done,
    /// A read found nothing.
    Miss,
    /// `check` found something worth a look.
    Warning,
    Error,
}

impl Outcome {
    /// The exit code: a miss is `1` only when `strict`.
    pub fn code(self, strict: bool) -> ExitCode {
        ExitCode::from(match self {
            Outcome::Done => 0,
            Outcome::Miss => u8::from(strict),
            Outcome::Warning => 1,
            Outcome::Error => 2,
        })
    }

    /// A `check` verdict's level: `FAIL` an error, `WARN` a warning, any
    /// other level (`PASS`, `INFO`) done.
    pub fn of_level(level: &str) -> Outcome {
        match level {
            "FAIL" => Outcome::Error,
            "WARN" => Outcome::Warning,
            _ => Outcome::Done,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Outcome;
    use std::process::ExitCode;

    #[test]
    fn a_miss_is_one_only_when_strict() {
        assert_eq!(Outcome::Miss.code(false), ExitCode::from(0));
        assert_eq!(Outcome::Miss.code(true), ExitCode::from(1));
        assert_eq!(Outcome::Done.code(true), ExitCode::from(0));
        assert_eq!(Outcome::Error.code(false), ExitCode::from(2));
    }

    #[test]
    fn the_worst_outcome_wins() {
        assert_eq!(Outcome::Done.max(Outcome::Miss), Outcome::Miss);
        assert_eq!(Outcome::Warning.max(Outcome::Error), Outcome::Error);
        assert_eq!(Outcome::of_level("WARN"), Outcome::Warning);
        assert_eq!(Outcome::of_level("INFO"), Outcome::Done);
    }
}
