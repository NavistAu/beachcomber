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
    let (socket_path, socket_source) = config.resolve_socket_path_with_source();
    let spawn_no_reap = matches!(socket_source, crate::config::SocketPathSource::EnvVar);

    if let Err(e) = crate::daemon::ensure_daemon(&socket_path, spawn_no_reap) {
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

    let client = crate::client::Client::new(socket_path);
    match client.status() {
        Ok(rows) => {
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

            // Canon provider_source.md invariant 16 and singleton.md
            // invariant 12: watch and reaper
            // degradation are observable via `comb status`. Human
            // preset only, and on stderr so machine formats stay
            // parseable.
            if preset == "human"
                && let Ok(resp) = client.introspect("daemon", None)
                && resp.ok
                && let Some(data) = resp.data.as_ref()
            {
                let glyph = if ascii { "!" } else { "⚠" };
                if let Some(backend) = data.get("watch_backend").and_then(|v| v.as_str())
                    && backend != "native"
                {
                    eprintln!(
                        "{glyph} watch backend: {backend} — kernel fs events undelivered; watch invalidation degraded"
                    );
                }
                if let Some(reaper) = data.get("reaper").filter(|r| !r.is_null()) {
                    let armed = reaper
                        .get("armed")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let confined =
                        reaper.get("visibility").and_then(|v| v.as_str()) == Some("confined");
                    let denied = reaper
                        .get("kill_denied")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    if armed && confined {
                        eprintln!(
                            "{glyph} reaper visibility degraded: daemon runs confined — orphan daemons outside its view are not policed"
                        );
                    }
                    if armed && denied > 0 {
                        eprintln!("{glyph} reaper: {denied} kill attempt(s) denied by the OS");
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::from(2)
        }
    }
}
