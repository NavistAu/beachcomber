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
//! The result prints through `libbeachcomber::render::render_value` — the
//! same renderer `comb get -f text` uses.

use crate::cli::exit::Outcome;
use crate::config::Config;
use libbeachcomber::resolve::{Inputs, Slot, eval};

/// `path` is what `main.rs` already settled: `None` for `--global`, otherwise
/// `--path`, the trailing positional, or the current directory. A render's
/// `exec()`, `lup()` and `read` run in the directory it addresses, `$HOME` for
/// the global entry — the resolver's rule, the same for every surface.
pub fn run_eval(config: &Config, template: &str, path: Option<&str>) -> Outcome {
    let (socket_path, socket_source) = config.resolve_socket_path_with_source();
    let spawn_no_reap = socket_source.no_reap();

    // `comb` always requires the daemon (design §2, outcome 22).
    if let Err(e) = crate::daemon::ensure_daemon(&socket_path, spawn_no_reap) {
        crate::cli::report::daemon_start(e);
        return Outcome::Error;
    }
    let mut session = match crate::client::Client::new(socket_path).connect() {
        Ok(s) => s,
        Err(e) => {
            crate::cli::report::daemon_unreachable(e);
            return Outcome::Error;
        }
    };

    let env = crate::cli::shell_env();
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let inputs = Inputs {
        cwd: &cwd,
        env: &env,
        slot: path.map_or(Slot::Global, |p| Slot::Path(p.to_string())),
        force: false,
        wait: false,
        allow_exec: true,
    };

    match eval(template, &inputs, &mut session) {
        // An absent value prints nothing, and is a miss, as for `comb get`.
        Ok(value) => {
            print!(
                "{}",
                libbeachcomber::render::render_value(
                    "",
                    value.as_ref(),
                    libbeachcomber::render::Format::Text
                )
            );
            if value.is_some() {
                Outcome::Done
            } else {
                Outcome::Miss
            }
        }
        Err(e) => {
            crate::cli::report::resolve_error("<eval>", &e);
            Outcome::Error
        }
    }
}
