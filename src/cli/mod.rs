pub mod commands;
pub mod config_origins;
pub mod connect;
pub mod exit;
pub mod output_format;
pub mod report;
pub mod status_format;
pub mod style;

/// The calling shell's environment: [`libbeachcomber::env::process`], the one
/// function every edge reads ambient state with.
pub fn shell_env() -> std::collections::HashMap<String, String> {
    libbeachcomber::env::process()
}
