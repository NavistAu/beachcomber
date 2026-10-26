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

use libbeachcomber::resolve::{Inputs, Slot};
use std::sync::atomic::{AtomicBool, Ordering};

static NO_EXEC: AtomicBool = AtomicBool::new(false);

/// Records the global `--no-exec` flag, once, as `main` has parsed it.
pub fn set_no_exec(no_exec: bool) {
    NO_EXEC.store(no_exec, Ordering::Relaxed);
}

/// The one place the CLI builds a resolution's [`Inputs`]: the caller's
/// directory and environment, which slot is addressed, the read's flags, and
/// whether a template may call `exec()` (on unless `--no-exec` was given).
pub fn inputs<'a>(
    cwd: &'a str,
    env: &'a std::collections::HashMap<String, String>,
    slot: Slot,
    force: bool,
    wait: bool,
) -> Inputs<'a> {
    Inputs {
        cwd,
        env,
        slot,
        force,
        wait,
        allow_exec: !NO_EXEC.load(Ordering::Relaxed),
        now: std::time::SystemTime::now(),
    }
}
