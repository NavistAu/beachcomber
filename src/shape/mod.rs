//! The config shape: what a Source's lines and the `_` prototypes mean,
//! resolved once at load.
//!
//! - [`decorators`] renders a Source's `return` line once, with recording
//!   filters, and turns the calls into a schedule.
//! - [`prototype`] finds the line a table applies: its own, else the nearest
//!   `_`'s, whole.
//! - [`build`] composes the config Sources of `[daemon.providers]`: prototypes,
//!   schedules and the rules between Sources.
//! - [`provider`] holds a provider's own lines (`template`, `result`, `return`)
//!   and renders them.
//! - [`seeded`] reads the two roots a configuration file seeds the cache from.

mod build;
pub mod decorators;
pub mod prototype;
pub mod provider;
pub mod seeded;

pub use build::{Built, PrototypeLines, ProviderDecl, Retune, build, prototype_lines};
