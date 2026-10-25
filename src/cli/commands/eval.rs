//! Handler for the `eval` subcommand.
//!
//! `eval` evaluates a value expression in either of the two forms canon
//! `field_resolution.md` (invariant 14) defines: exactly one `{{ expr }}`
//! (which keeps the expression's natural type), or literal text and/or
//! several tags (which is string-valued — a source with no tags at all is
//! its literal text).
//!
//! `eval` is a client of [`libbeachcomber::resolve`], exactly as `comb get`
//! is: every ref inside the template resolves through the one resolver, with
//! this process's environment and directory and `exec()` enabled, so
//! `comb eval '{{ p.f }}'` and `comb get p.f` agree by construction. `comb`
//! always requires the daemon (design §2) — there is no daemon-skip path.
//!
//! The result prints through `libbeachcomber::render::render_data` — the
//! same renderer `comb get -f text` uses.

use crate::config::Config;
use libbeachcomber::resolve::{Inputs, ResolveError, Slot, eval};
use std::process::ExitCode;

/// `path` is what `main.rs` already settled: `None` for `--global`, otherwise
/// `--path`, the trailing positional, or the current directory. A render's
/// `exec()`, `lup()` and `read` run in that directory, or in the current one
/// for the global entry.
pub fn run_eval(config: &Config, template: &str, path: Option<&str>) -> ExitCode {
    let (socket_path, socket_source) = config.resolve_socket_path_with_source();
    let spawn_no_reap = matches!(socket_source, crate::config::SocketPathSource::EnvVar);

    // `comb` always requires the daemon (design §2, outcome 22).
    if let Err(e) = crate::daemon::ensure_daemon(&socket_path, spawn_no_reap) {
        eprintln!("Failed to start daemon: {e}");
        return ExitCode::from(2);
    }
    let mut session = match crate::client::Client::new(socket_path).connect() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error: {e}");
            return ExitCode::from(2);
        }
    };

    let env = crate::cli::shell_env();
    let cwd = match path {
        Some(p) => p.to_string(),
        None => std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    let inputs = Inputs {
        cwd: &cwd,
        env: &env,
        slot: path.map_or(Slot::Global, |p| Slot::Path(p.to_string())),
        force: false,
        wait: false,
        allow_exec: true,
    };

    match eval(template, &inputs, &mut session) {
        // An absent value prints nothing, as a miss does for `comb get`.
        Ok(value) => {
            print!(
                "{}",
                libbeachcomber::render::render_data(value.as_ref(), "text")
            );
            ExitCode::SUCCESS
        }
        // The daemon's own words for a key it refused, or the transport's.
        Err(ResolveError::Fetch(e)) => {
            eprintln!("Error: {e}");
            ExitCode::from(2)
        }
        // A config or runtime failure already names its field.
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}
