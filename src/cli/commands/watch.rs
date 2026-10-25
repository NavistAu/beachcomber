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

use crate::cli::exit::Outcome;
use crate::cli::output_format::{Format, template_locals};
use crate::config::Config;
use libbeachcomber::render::{Format as ValueFormat, render_frame, render_value};
use libbeachcomber::resolve::{Slot, eval_with};
use libbeachcomber::watch::{ResolvedWatch, WatchOutcome};

/// `path` is what `main.rs` already settled: `None` for `--global`, otherwise
/// `--path`, the trailing positional, or the current directory.
pub fn run_watch(config: &Config, key: &str, path: Option<&str>, format: Format) -> Outcome {
    let socket_path = match crate::cli::connect::connect(config) {
        Ok(socket) => socket,
        Err(outcome) => return outcome,
    };

    let client = crate::client::build_client(&socket_path);
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let env = crate::cli::shell_env();
    let slot = match path {
        Some(p) => Slot::Path(p.to_string()),
        None => Slot::Global,
    };
    // A watch is a subscription, not a demand: it never forces an execution
    // or waits on one, it reports what the value becomes.
    let inputs = crate::cli::inputs(&cwd, &env, slot, false, false);

    // A `-f` template renders each frame through the resolver, which reads
    // what it names over a session of its own.
    let mut session = match &format {
        Format::Template(_) => match crate::client::Client::new(socket_path.clone()).connect() {
            Ok(session) => Some(session),
            Err(e) => {
                crate::cli::report::daemon_unreachable(e);
                return Outcome::Error;
            }
        },
        Format::Preset(_) => None,
    };

    let mut watch = match ResolvedWatch::open(&client, key, &inputs) {
        Ok(w) => w,
        Err(e) => {
            crate::cli::report::watch_error(key, &e);
            return Outcome::Error;
        }
    };

    loop {
        match watch.next(None) {
            Ok(WatchOutcome::Value(Some(value))) => {
                let frame = match (&format, session.as_mut()) {
                    (Format::Template(template), Some(session)) => {
                        let locals = template_locals(&[(key, Some(value))]);
                        eval_with(template, "-f", &inputs, session, &locals)
                            .map(|v| render_value("", v.as_ref(), ValueFormat::Text) + "\n")
                            .map_err(|e| e.to_string())
                    }
                    (Format::Preset(preset), _) => Ok(render_frame(
                        key,
                        &value,
                        preset
                            .value_format()
                            .expect("watch's formats are value formats"),
                    )),
                    (Format::Template(_), None) => unreachable!("a template opens its session"),
                };
                if let Err(e) = frame.map(|frame| print!("{frame}")) {
                    crate::cli::report::error_named(e);
                    return Outcome::Error;
                }
            }
            // A miss carries no value, so there is nothing to print. The
            // watch stays open: a later `put` can give the key a value.
            Ok(WatchOutcome::Value(None)) => {}
            // Nothing left that could ever answer.
            Ok(WatchOutcome::Eof) => return Outcome::Done,
            // `next(None)` has no deadline and this watch's cancel handle is
            // never set, so neither is reachable today — but looping here
            // instead of returning would busy-spin forever if either ever
            // became reachable, so treat both as the watch being over.
            Ok(WatchOutcome::Timeout | WatchOutcome::Cancelled) => return Outcome::Done,
            Err(e) => {
                crate::cli::report::watch_error(key, &e);
                return Outcome::Error;
            }
        }
    }
}
