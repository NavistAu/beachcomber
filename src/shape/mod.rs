//! The config shape: what a Source's lines and the `_` prototypes mean,
//! resolved once at load.
//!
//! - [`decorators`] renders a Source's `return` line once, with recording
//!   filters, and turns the calls into a schedule.
//! - [`prototype`] finds the line a table applies: its own, else the nearest
//!   `_`'s, whole.

pub mod decorators;
pub mod prototype;
