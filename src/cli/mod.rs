pub mod commands;
pub mod config_origins;
pub mod connect;
pub mod exit;
pub mod output_format;
pub mod report;
pub mod status_format;
pub mod style;

use std::collections::HashMap;

/// The calling shell's environment, as `libbeachcomber::resolve::Inputs`
/// takes it. Read lossily: a value that is not UTF-8 is converted with
/// replacement characters rather than aborting the command (`std::env::vars`
/// panics on one), and an entry whose *name* is not UTF-8 is skipped — no
/// `env.X` can spell it, so nothing could ever ask for it.
pub fn shell_env() -> HashMap<String, String> {
    std::env::vars_os()
        .filter_map(|(k, v)| {
            let key = k.into_string().ok()?;
            Some((key, v.to_string_lossy().into_owned()))
        })
        .collect()
}
