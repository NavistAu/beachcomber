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
//! `field_resolution.md` invariant 8). A frame is one line. A miss is what
//! `get` prints for one: `null` under `-f json`, a template's own rendering
//! under `-f '<template>'` (through the one `render_template` `get` uses),
//! and no line at all under every other format.

use crate::cli::exit::Outcome;
use crate::cli::output_format::{Format, render_template};
use crate::config::Config;
use libbeachcomber::render::render_frame;
use libbeachcomber::resolve::Slot;
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
    let inputs = crate::cli::inputs(&cwd, &env, slot, false, libbeachcomber::Wait::Budget);

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
            // A miss is a frame as `get` prints one: `null` under json, no
            // line otherwise. The watch stays open either way: a later `put`
            // can give the key a value.
            Ok(WatchOutcome::Value(value)) => {
                let frame = match (&format, session.as_mut()) {
                    // The frame is a line: the template's text, ended.
                    (Format::Template(template), Some(session)) => {
                        render_template(template, &[(key, value)], &inputs, session)
                            .map(|text| text + "\n")
                    }
                    (Format::Preset(preset), _) => Ok(render_frame(
                        key,
                        value.as_ref(),
                        preset
                            .value_format()
                            .expect("watch's formats are value formats"),
                    )),
                    (Format::Template(_), None) => {
                        unreachable!("a template opens its session")
                    }
                };
                if let Err(e) = frame.map(|frame| print!("{frame}")) {
                    crate::cli::report::error_named(e);
                    return Outcome::Error;
                }
            }
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
