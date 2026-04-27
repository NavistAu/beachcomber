//! Command handlers extracted from `src/main.rs`.
//!
//! Each handler accepts a parsed clap subcommand value plus a `&Config`
//! and returns `ExitCode`. Handlers must not call `std::process::exit`;
//! that decision belongs to `main`.

pub mod check;
pub mod daemon;
pub mod eval;
pub mod get;
pub mod init;
pub mod kill;
pub mod put;
pub mod status;
pub mod watch;
