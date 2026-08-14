//! Handler for the `watch` subcommand.
//!
//! Moved from `src/main.rs` in Task 2.5.

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

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime");
    rt.block_on(async {
        let client = crate::client::Client::new(socket_path);
        let mut session = match client.connect().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error: {e}");
                return ExitCode::from(2);
            }
        };

        let server_fmt = match &format {
            OutputFormat::Text => Some("text"),
            OutputFormat::Sh => Some("sh"),
            _ => None,
        };
        if let Err(e) = session.watch(key, path, server_fmt).await {
            eprintln!("Error: {e}");
            return ExitCode::from(2);
        }

        // For server-side formats, stream lines directly.
        // For client-side formats, each watch line is a JSON response we need to reformat.
        loop {
            match session.read_watch_line().await {
                Ok(Some(line)) => match &format {
                    OutputFormat::Json | OutputFormat::Text | OutputFormat::Sh => {
                        print!("{line}");
                    }
                    _ => {
                        if let Ok(response) =
                            serde_json::from_str::<crate::protocol::Response>(&line)
                            && let Some(data) = &response.data
                        {
                            match &format {
                                OutputFormat::Csv => {
                                    println!("{}", format_sv(data, ",", false));
                                }
                                OutputFormat::Tsv => {
                                    println!("{}", format_sv(data, "\t", false));
                                }
                                OutputFormat::CsvHeader => {
                                    println!("{}", format_sv(data, ",", true));
                                }
                                OutputFormat::TsvHeader => {
                                    println!("{}", format_sv(data, "\t", true));
                                }
                                OutputFormat::Fmt(template) => {
                                    match render_fmt_template_json(template, data) {
                                        Ok(rendered) => println!("{}", rendered),
                                        Err(e) => {
                                            eprintln!("Template error: {e}");
                                            return ExitCode::from(2);
                                        }
                                    }
                                }
                                _ => unreachable!(),
                            }
                        }
                    }
                },
                Ok(None) => break,
                Err(e) => {
                    eprintln!("Error: {e}");
                    return ExitCode::from(2);
                }
            }
        }

        ExitCode::SUCCESS
    })
}
