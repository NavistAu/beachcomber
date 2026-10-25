//! The process environment, read at an edge.
//!
//! The resolver reads no ambient state: it renders with the `env` it is
//! given. An edge that has no caller-supplied map (the `comb` CLI, and the C
//! ABI when a call passes none) makes one here, so every surface that means
//! "the calling process's environment" means this function.

use std::collections::HashMap;

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
    use super::process;

    #[test]
    fn the_map_holds_the_process_variables() {
        let map = process();
        assert_eq!(
            map.get("PATH").map(String::as_str),
            std::env::var("PATH").ok().as_deref()
        );
    }
}
