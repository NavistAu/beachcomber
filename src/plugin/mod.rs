//! Plugins compiled into comb. There is one, [`sys`].
//!
//! A compiled-in plugin is a table of the same `extern "C"` symbols a plugin
//! library exports, and [`StaticLibrary`] is the [`LoadedLibrary`] over it: a
//! call finds the symbol in the table where a library's is found by `dlsym`,
//! and goes on down the one path every plugin call takes
//! ([`call_symbol`]). So `plugin('sys', 'hostname')` runs what a user's
//! `plugin('mine', 'f')` runs, from the engine function to the C call.

pub mod sys;

use crate::boundaries::library::{JsonFn, LoadedLibrary, call_symbol};

/// The name `sys` is answered by, before any search path: reserved.
pub const SYS: &str = "sys";

/// A plugin compiled in: its symbols, by name.
pub struct StaticLibrary {
    symbols: &'static [(&'static str, JsonFn)],
}

impl StaticLibrary {
    /// The `sys` plugin.
    pub fn sys() -> Self {
        Self {
            symbols: sys::SYMBOLS,
        }
    }
}

impl LoadedLibrary for StaticLibrary {
    /// Its functions are reentrant, so it takes no lock: two threads may call
    /// into it at once.
    fn call_json(&self, symbol: String, input: String) -> Result<String, String> {
        let f = self
            .symbols
            .iter()
            .find(|(name, _)| *name == symbol)
            .map(|(_, f)| *f)
            .ok_or_else(|| "symbol not found".to_string())?;
        call_symbol(f, input)
    }

    /// The table's address: no loaded image is it.
    fn identity(&self) -> usize {
        self.symbols.as_ptr() as usize
    }
}
