//! The process environment, read at an edge.
//!
//! The resolver reads no ambient state: it renders with the `env` it is
//! given. An edge that has no caller-supplied map (the `comb` CLI, and the C
//! ABI when a call passes none) makes one here, so every surface that means
//! "the calling process's environment" means this function.

use std::collections::HashMap;
use std::sync::OnceLock;

/// An environment as `resolve::Inputs` reads it: a map, read only when a
/// resolution needs it. A plain cached value never does, so an edge that
/// would read the process environment for every call pays nothing.
pub trait Vars: Sync {
    /// The variables, by name.
    fn vars(&self) -> &HashMap<String, String>;
}

impl Vars for HashMap<String, String> {
    fn vars(&self) -> &HashMap<String, String> {
        self
    }
}

/// The calling process's environment ([`process`]), read on first use and
/// kept for the rest of the value's life: make one per call, so each call
/// sees the environment as it is when that call runs.
#[derive(Default)]
pub struct Lazy(OnceLock<HashMap<String, String>>);

impl Vars for Lazy {
    fn vars(&self) -> &HashMap<String, String> {
        self.0.get_or_init(process)
    }
}

/// The calling process's environment, as `resolve::Inputs` takes it.
///
/// Read lossily: a value that is not UTF-8 is converted with replacement
/// characters rather than aborting the call (`std::env::vars` panics on
/// one), and an entry whose *name* is not UTF-8 is skipped: no `env.X` can
/// spell it, so nothing could ever ask for it.
pub fn process() -> HashMap<String, String> {
    std::env::vars_os()
        .filter_map(|(k, v)| {
            let key = k.into_string().ok()?;
            Some((key, v.to_string_lossy().into_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Lazy, Vars, process};

    #[test]
    fn the_map_holds_the_process_variables() {
        let map = process();
        assert_eq!(
            map.get("PATH").map(String::as_str),
            std::env::var("PATH").ok().as_deref()
        );
    }

    #[test]
    fn a_lazy_env_reads_the_process_variables() {
        assert_eq!(Lazy::default().vars(), &process());
    }
}
