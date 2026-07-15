//! Handler for the `status` (`s`) subcommand.
//!
//! Moved from `src/main.rs` in Task 2.3.

use crate::cli::status_format::{
    ColorMode, RenderOpts, apply_filters, apply_sort, render_preset, resolve_color,
    resolve_max_width,
};
use crate::config::Config;
use std::io::IsTerminal;
use std::process::ExitCode;

#[allow(clippy::too_many_arguments)]
pub fn run_status(
    config: &Config,
    format: Option<&str>,
    filters: &[String],
    sort_col: &str,
    no_trunc: bool,
    max_width_arg: Option<&str>,
    color_arg: &str,
    ascii: bool,
) -> ExitCode {
    let socket_path = config.resolve_socket_path();

    if let Err(e) = crate::daemon::ensure_daemon(&socket_path) {
        eprintln!("Failed to start daemon: {e}");
        return ExitCode::from(2);
    }

    let is_tty = std::io::stdout().is_terminal();
    let mode = match color_arg {
        "always" => ColorMode::Always,
        "never" => ColorMode::Never,
        _ => ColorMode::Auto,
    };
    let no_color_env = std::env::var("NO_COLOR").is_ok();
    let watch_env = std::env::var("WATCH_INTERVAL").is_ok();
    let color = resolve_color(mode, no_color_env, is_tty, watch_env);

    let cols = terminal_size::terminal_size().map(|(w, _)| w.0 as usize);
    let resolved_max_width = resolve_max_width(max_width_arg, cols);

    let preset = format.unwrap_or("human");
    let opts = RenderOpts {
        is_tty,
        no_color: !color,
        max_width: if no_trunc {
            None
        } else {
            Some(resolved_max_width)
        },
        no_trunc,
        ascii,
    };

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    rt.block_on(async {
        let client = crate::client::Client::new(socket_path);
        match client.send_raw(serde_json::json!({"op": "status"})).await {
            Ok(response) => {
                if response.ok {
                    let rows: Vec<crate::cache::CacheRow> = response
                        .data
                        .as_ref()
                        .and_then(|d| serde_json::from_value(d.clone()).ok())
                        .unwrap_or_default();
                    let rows = apply_filters(rows, filters).unwrap_or_else(|e| {
                        eprintln!("filter error: {e}");
                        std::process::exit(2);
                    });
                    let rows = apply_sort(rows, sort_col).unwrap_or_else(|e| {
                        eprintln!("sort error: {e}");
                        std::process::exit(2);
                    });
                    let out = render_preset(preset, &rows, &opts);
                    print!("{out}");

                    // Canon singleton.md invariant 12: watch degradation is
                    // observable via `comb status`. Human preset only, and on
                    // stderr so machine formats stay parseable.
                    if preset == "human"
                        && let Ok(resp) = client
                            .send_raw(serde_json::json!({"op": "introspect", "subject": "daemon"}))
                            .await
                        && resp.ok
                        && let Some(backend) = resp
                            .data
                            .as_ref()
                            .and_then(|d| d.get("watch_backend"))
                            .and_then(|v| v.as_str())
                        && backend != "native"
                    {
                        let glyph = if ascii { "!" } else { "⚠" };
                        eprintln!(
                            "{glyph} watch backend: {backend} — kernel fs events undelivered; watch invalidation degraded"
                        );
                    }
                    ExitCode::SUCCESS
                } else {
                    eprintln!("Error: {}", response.error.unwrap_or_default());
                    ExitCode::from(2)
                }
            }
            Err(e) => {
                eprintln!("Error: {e}");
                ExitCode::from(2)
            }
        }
    })
}
