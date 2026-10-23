//! The config shape: what a Source's lines and the `_` prototypes mean,
//! resolved once at load.
//!
//! - [`decorators`] renders a Source's `return` line once, with recording
//!   filters, and turns the calls into a schedule.
//! - [`prototype`] finds the line a table applies: its own, else the nearest
//!   `_`'s, whole.
//! - [`seeded`] reads the two roots a configuration file seeds the cache from.

pub mod decorators;
pub mod prototype;
pub mod seeded;
