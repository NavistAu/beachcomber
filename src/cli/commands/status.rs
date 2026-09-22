//! Handler for the `status` (`s`) subcommand.
//!
//! Moved from `src/main.rs` in Task 2.3.

use crate::cache::{CacheRow, RowKind};
use crate::cli::status_format::{
    ColorMode, RenderOpts, apply_filters, apply_sort, default_depth, flatten_rows, render_preset,
    resolve_color, resolve_max_width,
};
use crate::config::Config;
use libbeachcomber::resolve::{Inputs, Slot, resolve};
use std::collections::HashMap;
use std::io::IsTerminal;
use std::process::ExitCode;

/// The namespace facet holding value expressions (canon
/// `field_resolution.md` "Facet").
const TEMPLATE_FACET: &str = ":template";

/// The SOURCE a rendered row reports: nothing produced it but this
/// resolution, here.
const TEMPLATE_SOURCE: &str = "template";

/// One rendered row per template leaf the caller's directory can see.
///
/// The daemon holds templates as data and never evaluates them (canon
/// `field_resolution.md` invariant 7), so `status` shows raw expressions
/// where a user wants values. Each leaf under a `<p>:template` row visible
/// from `cwd` — the row's own path, an ancestor of it up to `$HOME`, or the
/// global entry (canon invariant 2) — is resolved as `<p>.<leaf>` would be by
/// `comb get`, with `exec()` on: `comb status` is one of the surfaces that
/// opted in (canon `field_resolution.md` invariant 15).
///
/// A leaf that resolves to nothing is not a row. A leaf that fails carries
/// its failure as its value — the cell is where the user looks for it, and a
/// broken expression is exactly what this view exists to show.
fn render_template_rows(
    rows: &[CacheRow],
    cwd: &str,
    env: &HashMap<String, String>,
    client: &libbeachcomber::Client,
    filters: &[String],
) -> Vec<CacheRow> {
    let chain = crate::query::path_chain(Some(cwd));
    let mut addrs: Vec<(String, String)> = Vec::new();
    for row in rows {
        let Some(provider) = row.provider.strip_suffix(TEMPLATE_FACET) else {
            continue;
        };
        // `--filter provider=` rules a provider out outright, so its
        // templates are skipped here rather than resolved — and any
        // exec() in them run — only to be dropped once apply_filters runs
        // post-render. Field/path predicates need the rendered, flattened
        // rows to evaluate (see the comment in `run_status`), so only the
        // provider predicate is checked this early.
        if !crate::cli::status_format::provider_could_pass(filters, provider) {
            continue;
        }
        let visible = chain.iter().any(|l| l.as_deref() == row.path.as_deref());
        if visible {
            collect_leaves(provider, &row.field, &row.value, &mut addrs);
        }
    }
    // The same leaf may be declared at several points on the chain; the
    // resolver decides which one wins, so one address is one row.
    addrs.sort();
    addrs.dedup();

    let inputs = Inputs {
        cwd,
        env,
        slot: Slot::Cwd,
        force: false,
        wait: false,
        allow_exec: true,
    };
    let mut fetch = libbeachcomber::ClientFetch::new(client);
    let mut out = Vec::new();
    for (provider, field) in addrs {
        let value = match resolve(&format!("{provider}.{field}"), &inputs, &mut fetch) {
            Ok(Some(value)) => value,
            Ok(None) => continue,
            Err(e) => serde_json::Value::String(e.to_string()),
        };
        out.push(CacheRow {
            provider,
            path: Some(cwd.to_string()),
            source: TEMPLATE_SOURCE.to_string(),
            field,
            value,
            age_ms: 0,
            stale: false,
            kind: Some(RowKind::Template),
            poll_interval_secs: None,
            keep_alive_polls: None,
            fsevents_reinstate: None,
            polls_elapsed: None,
            next_poll_in_secs: None,
            failure: None,
        });
    }
    out
}

/// Every leaf address under `value`, as `(provider, dotted field path)`.
/// An interior node of a template tree is not a value expression — only its
/// leaves are.
fn collect_leaves(
    provider: &str,
    field: &str,
    value: &serde_json::Value,
    out: &mut Vec<(String, String)>,
) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                collect_leaves(provider, &format!("{field}.{key}"), child, out);
            }
        }
        serde_json::Value::String(_) => out.push((provider.to_string(), field.to_string())),
        _ => {}
    }
}

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
    depth: Option<usize>,
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
    let color = resolve_color(mode, no_color_env, is_tty);

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
        depth,
    };

    let client = crate::client::Client::new(socket_path.clone());
    match client.status() {
        Ok(mut rows) => {
            // The daemon never evaluates a template, so the rendered rows are
            // added here, from this shell's directory and environment. A
            // template resolves relative to `cwd` (canon invariant 2); with
            // no cwd to resolve against, there is no correct directory to
            // stamp a rendered row with, so rendering is skipped rather than
            // resolving every leaf as if the caller stood at "" — which
            // stamped rows with `path: Some("")`, an empty PATH cell that
            // reads as a lie. The daemon's own rows still print.
            if let Ok(cwd) = std::env::current_dir() {
                let cwd = cwd.to_string_lossy().into_owned();
                let rendered = render_template_rows(
                    &rows,
                    &cwd,
                    &crate::cli::shell_env(),
                    &crate::client::build_client(&socket_path),
                    filters,
                );
                rows.extend(rendered);
            }

            // Destructure before filtering and sorting, so both operate on the
            // field names the user can actually see in the output.
            let rows = flatten_rows(rows, opts.depth.unwrap_or_else(|| default_depth(preset)));
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
