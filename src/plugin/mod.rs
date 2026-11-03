//! Plugins compiled into comb: [`sys`] and [`git`].
//!
//! A compiled-in plugin is a table of the same `extern "C"` symbols a plugin
//! library exports, and [`StaticLibrary`] is the [`LoadedLibrary`] over it: a
//! call finds the symbol in the table where a library's is found by `dlsym`,
//! and goes on down the one path every plugin call takes
//! ([`call_symbol`]). So `plugin('sys', 'hostname')` runs what a user's
//! `plugin('mine', 'f')` runs, from the engine function to the C call.
//!
//! An answer is written into a thread-local string of its symbol's and its
//! pointer returned: valid until the next call of the same symbol on the same
//! thread, which is stricter than the contract and needs no lock. So what a
//! symbol wraps must be reentrant: two threads may call at once. A symbol must
//! not unwind, as a library's must not: a panic out of an `extern "C" fn`
//! aborts.

use crate::boundaries::library::{JsonFn, LoadedLibrary, call_symbol};
use serde_json::Value as Json;
use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char};
use std::sync::Arc;
use std::thread::LocalKey;

/// A plugin's symbols, by name, sorted.
type Symbols = &'static [(&'static str, JsonFn)];

/// What a symbol answers: a JSON object.
type Facts = serde_json::Map<String, Json>;

/// Each symbol: an `extern "C"` function in `mod symbol` answering
/// `$adapt($facts, args)` into a thread-local string of its own; and the
/// table of them, `SYMBOLS`.
macro_rules! symbols {
    ($adapt:ident; $($name:ident => $facts:path),* $(,)?) => {
        /// The symbols themselves.
        mod symbol {
            use super::*;
            $(
                /// # Safety
                ///
                /// `input` is `NULL` or a NUL-terminated string, as the
                /// plugin contract has it.
                pub(super) unsafe extern "C" fn $name(
                    input: *const std::ffi::c_char,
                ) -> *const std::ffi::c_char {
                    thread_local! {
                        static OUT: std::cell::RefCell<std::ffi::CString> =
                            std::cell::RefCell::new(std::ffi::CString::default());
                    }
                    // SAFETY: as the function's own contract.
                    unsafe { $crate::plugin::answer(&OUT, input, |args| $adapt($facts, args)) }
                }
            )*
        }

        /// Every symbol, by name, sorted.
        pub const SYMBOLS: &[(&str, $crate::boundaries::library::JsonFn)] =
            &[$((stringify!($name), symbol::$name as $crate::boundaries::library::JsonFn)),*];
    };
}
use symbols;

pub mod git;
pub mod sys;

/// The plugins compiled in, each answered by its name before any search path:
/// these names are reserved.
const COMPILED_IN: &[(&str, Symbols)] = &[("git", git::SYMBOLS), ("sys", sys::SYMBOLS)];

/// `(name, library)` of every compiled-in plugin.
pub fn compiled_in() -> impl Iterator<Item = (&'static str, Arc<dyn LoadedLibrary>)> {
    COMPILED_IN.iter().map(|(name, symbols)| {
        let lib: Arc<dyn LoadedLibrary> = Arc::new(StaticLibrary { symbols });
        (*name, lib)
    })
}

/// The reserved names: what a library on the search path may not be called.
pub fn reserved_names() -> impl Iterator<Item = &'static str> {
    COMPILED_IN.iter().map(|(name, _)| *name)
}

/// `facts(args)` as JSON text in `out`, its pointer; `NULL` when the input is
/// not a JSON array or there are no facts.
///
/// # Safety
///
/// `input` is `NULL` or a NUL-terminated string.
unsafe fn answer(
    out: &'static LocalKey<RefCell<CString>>,
    input: *const c_char,
    facts: impl FnOnce(Vec<Json>) -> Option<Facts>,
) -> *const c_char {
    if input.is_null() {
        return std::ptr::null();
    }
    // SAFETY: a NUL-terminated string, by the caller's contract.
    let args = unsafe { CStr::from_ptr(input) }.to_bytes();
    let Ok(args) = serde_json::from_slice::<Vec<Json>>(args) else {
        return std::ptr::null();
    };
    let Some(facts) = facts(args) else {
        return std::ptr::null();
    };
    // JSON text holds no NUL: serde_json writes one in a string as `\u0000`.
    let Ok(text) = CString::new(Json::Object(facts).to_string()) else {
        return std::ptr::null();
    };
    out.with(|slot| {
        let mut slot = slot.borrow_mut();
        *slot = text;
        slot.as_ptr()
    })
}

/// A plugin compiled in: its symbols, by name.
pub struct StaticLibrary {
    symbols: Symbols,
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
