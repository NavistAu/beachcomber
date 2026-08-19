//! Handler for the `watch` subcommand.
//!
//! Moved from `src/main.rs` in Task 2.5. Task 1.8 retired the daemon's
//! server-side `text`/`sh` rendering: the daemon now streams JSON frames
//! only, and every non-JSON output format is rendered locally from each
//! frame's `data`, reusing `libbeachcomber::render::render_data` -- the same
//! rendering `Client::get_formatted_with_flags` uses for `comb get`.

use crate::cli::format::render_fmt_template_json;
use crate::cli::output_format::{OutputFormat, format_sv};
use crate::config::Config;
use std::process::ExitCode;

pub fn run_watch(config: &Config, key: &str, path: Option<&str>, format: OutputFormat) -> ExitCode {
    let (socket_path, socket_source) = config.resolve_socket_path_with_source();
    let spawn_no_reap = matches!(socket_source, crate::config::SocketPathSource::EnvVar);

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

    if let Err(e) = session.watch(key, path) {
        eprintln!("Error: {e}");
        return ExitCode::from(2);
    }

    loop {
        match session.read_watch_event() {
            Ok(Some(response)) => {
                if !response.ok {
                    eprintln!(
                        "Error: {}",
                        response.error.as_deref().unwrap_or("unknown error")
                    );
                    continue;
                }
                match &format {
                    OutputFormat::Json => {
                        println!("{}", serde_json::to_string(&response).unwrap());
                    }
                    OutputFormat::Text | OutputFormat::Sh => {
                        println!(
                            "{}",
                            libbeachcomber::render::render_data(response.data.as_ref())
                        );
                    }
                    OutputFormat::Csv => {
                        if let Some(data) = &response.data {
                            println!("{}", format_sv(data, ",", false));
                        }
                    }
                    OutputFormat::Tsv => {
                        if let Some(data) = &response.data {
                            println!("{}", format_sv(data, "\t", false));
                        }
                    }
                    OutputFormat::CsvHeader => {
                        if let Some(data) = &response.data {
                            println!("{}", format_sv(data, ",", true));
                        }
                    }
                    OutputFormat::TsvHeader => {
                        if let Some(data) = &response.data {
                            println!("{}", format_sv(data, "\t", true));
                        }
                    }
                    OutputFormat::Fmt(template) => {
                        if let Some(data) = &response.data {
                            match render_fmt_template_json(template, data) {
                                Ok(rendered) => println!("{}", rendered),
                                Err(e) => {
                                    eprintln!("Template error: {e}");
                                    return ExitCode::from(2);
                                }
                            }
                        }
                    }
                }
            }
            Ok(None) => break,
            Err(e) => {
                eprintln!("Error: {e}");
                return ExitCode::from(2);
            }
        }
    }

    ExitCode::SUCCESS
}
