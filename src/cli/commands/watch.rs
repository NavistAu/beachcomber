//! Handler for the `watch` subcommand.
//!
//! `comb watch` is a client of [`libbeachcomber::watch`] the way `comb get`
//! is a client of `libbeachcomber::resolve`: it names a key, a path and a
//! format, makes sure the daemon is running, and hands the rest to the
//! library. Every frame it prints is a *value* — re-resolved by the library
//! over every key the resolution reads plus the provider's `<p>:template`,
//! so a template edited under the watch fires it just as a changed cache
//! record does. Under `text`/`sh`/`csv`/`tsv`/`-f fmt` a frame renders
//! exactly as `comb get` renders that same value; under `-f json` it is the
//! same value compacted to one line per frame, where `get` pretty-prints.
//!
//! This module only renders what the library emits, per output format —
//! reusing `libbeachcomber::render::render_data` for `text`/`sh` exactly as
//! `get` does, including the shell-quoting `-f sh` requires (canon
//! `field_resolution.md` invariant 8). A frame is one line; a miss is no
//! line at all, because there is no value to print.

use crate::cli::format::render_fmt_template_json;
use crate::cli::output_format::Format;
use crate::config::Config;
use libbeachcomber::render::render_frame;
use libbeachcomber::resolve::{Inputs, Slot};
use libbeachcomber::watch::{ResolvedWatch, WatchOutcome};
use serde_json::Value;
use std::process::ExitCode;

/// `path` is what `main.rs` already settled: `None` for `--global`, otherwise
/// `--path`, the trailing positional, or the current directory.
pub fn run_watch(config: &Config, key: &str, path: Option<&str>, format: Format) -> ExitCode {
    let (socket_path, socket_source) = config.resolve_socket_path_with_source();
    let spawn_no_reap = matches!(socket_source, crate::config::SocketPathSource::EnvVar);

    if let Err(e) = crate::daemon::ensure_daemon(&socket_path, spawn_no_reap) {
        eprintln!("Failed to start daemon: {e}");
        return ExitCode::from(2);
    }

    let client = crate::client::build_client(&socket_path);
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let env = crate::cli::shell_env();
    let slot = match path {
        Some(p) => Slot::Path(p.to_string()),
        None => Slot::Global,
    };
    let inputs = Inputs {
        cwd: &cwd,
        env: &env,
        slot,
        // A watch is a subscription, not a demand: it never forces an
        // execution or waits on one, it reports what the value becomes.
        force: false,
        wait: false,
        allow_exec: true,
    };

    let mut watch = match ResolvedWatch::open(&client, key, &inputs) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };

    loop {
        match watch.next(None) {
            Ok(WatchOutcome::Value(Some(value))) => {
                if let Err(e) = print_frame(key, &format, &value) {
                    eprintln!("{e}");
                    return ExitCode::from(2);
                }
            }
            // A miss carries no value, so there is nothing to print. The
            // watch stays open: a later `put` can give the key a value.
            Ok(WatchOutcome::Value(None)) => {}
            // Nothing left that could ever answer.
            Ok(WatchOutcome::Eof) => return ExitCode::SUCCESS,
            // `next(None)` has no deadline and this watch's cancel handle is
            // never set, so neither is reachable today — but looping here
            // instead of returning would busy-spin forever if either ever
            // became reachable, so treat both as the watch being over.
            Ok(WatchOutcome::Timeout | WatchOutcome::Cancelled) => return ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(2);
            }
        }
    }
}

/// One frame: the value as `comb get` prints it, on a line of its own, json
/// compacted to one line.
fn print_frame(key: &str, format: &Format, value: &Value) -> Result<(), String> {
    let frame = match format {
        Format::Template(template) => format!("{}\n", render_fmt_template_json(template, value)?),
        Format::Preset(preset) => render_frame(
            key,
            value,
            preset
                .value_format()
                .expect("watch's formats are value formats"),
        ),
    };
    print!("{frame}");
    Ok(())
}
