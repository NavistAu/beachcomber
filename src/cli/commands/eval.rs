//! Handler for the `eval` subcommand.
//!
//! `eval` evaluates a value expression in any of the three forms canon
//! `field_resolution.md` (invariant 14) defines: a bare expression, exactly one
//! `{{ expr }}` (which keeps the expression's natural type), or literal text
//! and/or several tags (which is string-valued). `libbeachcomber::eval` owns
//! all three; this handler only supplies what the library cannot reach on its
//! own — the calling shell's environment and the daemon.
//!
//! Resolution mirrors `comb get`'s layering:
//!
//! - `env.*` refs come from the calling shell's environment.
//! - virtual fields (e.g. `terraform.workspace`) are evaluated client-side.
//! - plain `provider.field` refs are fetched from the daemon.
//!
//! A source that needs no daemon-backed data never starts/contacts the daemon:
//! `eval::daemon_refs` returns the transitive closure of the daemon keys the
//! source needs (following virtual fields into their own dependencies), and an
//! empty closure means `ensure_daemon` is never called.
//!
//! The result prints through `libbeachcomber::render::render_data` — the same
//! renderer `comb get -f text` uses — so `comb eval '{{ p.f }}'` and
//! `comb get p.f` agree on how a value looks.

use crate::config::Config;
use libbeachcomber::eval;
use libbeachcomber::virtual_fields::{EvalContext, VirtualFields};
use std::collections::HashMap;
use std::process::ExitCode;

pub fn run_eval(config: &Config, template: &str, path: Option<&str>) -> ExitCode {
    let (socket_path, socket_source) = config.resolve_socket_path_with_source();
    let spawn_no_reap = matches!(socket_source, crate::config::SocketPathSource::EnvVar);
    let vf = VirtualFields::with_config_overrides(config.virtual_fields());

    // Every daemon key this source needs, virtual-field dependencies included.
    let refs = eval::daemon_refs(template, &vf);

    let daemon_data = if refs.is_empty() {
        // Nothing daemon-backed (env.* only, or pure-env virtual fields): never
        // start or contact a daemon.
        HashMap::new()
    } else {
        if let Err(e) = crate::daemon::ensure_daemon(&socket_path, spawn_no_reap) {
            eprintln!("Failed to start daemon: {e}");
            return ExitCode::from(2);
        }
        let client = crate::client::Client::new(socket_path);
        let mut session = match client.connect() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error: {e}");
                return ExitCode::from(2);
            }
        };
        if let Some(p) = path
            && let Err(e) = session.set_context(p)
        {
            eprintln!("Error: {e}");
            return ExitCode::from(2);
        }
        // A transport failure aborts the whole command; a cache miss is simply
        // absent from the map and evaluates falsy.
        let fetched = eval::fetch_daemon_data(&refs, |key| {
            session
                .get(key, None)
                .map(|resp| resp.data)
                .map_err(|e| format!("Error querying {key}: {e}"))
        });
        match fetched {
            Ok(d) => d,
            Err(msg) => {
                eprintln!("{msg}");
                return ExitCode::from(2);
            }
        }
    };

    let shell_env: HashMap<String, String> = std::env::vars().collect();
    let ctx = EvalContext {
        env_vars: &shell_env,
        daemon_data: &daemon_data,
    };

    match eval::evaluate(template, &vf, &ctx) {
        Ok(value) => {
            print!("{}", libbeachcomber::render::render_data(Some(&value)));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}
