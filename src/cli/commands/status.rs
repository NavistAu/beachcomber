//! Handler for the `status` (`s`) subcommand.
//!
//! Moved from `src/main.rs` in Task 2.3.

use crate::cache::{CacheRow, RowKind};
use crate::cli::output_format::{Format, Preset};
use crate::cli::status_format::{
    ColorMode, RenderOpts, apply_filters, apply_sort, default_depth, flatten_rows, render_preset,
    render_tree, resolve_color, resolve_max_width, row_base, row_facet,
};
use crate::config::Config;
use libbeachcomber::resolve::{Inputs, Slot, resolve_traced};
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
/// A leaf that resolves to nothing is a row with a null value, which the
/// default `value!=-` filter hides. A leaf that fails carries its failure as
/// its value — the cell is where the user looks for it, and a broken
/// expression is exactly what this view exists to show.
fn render_template_rows(
    rows: &[CacheRow],
    cwd: &str,
    env: &HashMap<String, String>,
    session: &mut crate::client::ClientSession,
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
    let mut out = Vec::new();
    for (provider, field) in addrs {
        // A rendered value has no age of its own: it is as old as the oldest
        // cached input it used, and has none when it used none.
        let (value, age_ms) = match resolve_traced(
            &libbeachcomber::key::join(&provider, &field),
            &inputs,
            session,
        ) {
            Ok(traced) => (
                traced.value.unwrap_or(serde_json::Value::Null),
                traced.oldest_input_ms,
            ),
            Err(e) => (serde_json::Value::String(e.to_string()), None),
        };
        out.push(CacheRow {
            shadowed: false,
            provider,
            path: Some(cwd.to_string()),
            source: TEMPLATE_SOURCE.to_string(),
            field,
            value,
            age_ms,
            stale: false,
            kind: Some(RowKind::Template),
            poll_interval_secs: None,
            poll_count: None,
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
                collect_leaves(provider, &libbeachcomber::key::push(field, key), child, out);
            }
        }
        serde_json::Value::String(_) => out.push((provider.to_string(), field.to_string())),
        _ => {}
    }
}

/// The presets `--tree` draws: the two tables, and `json` (a `layers` array
/// per row).
const TREE_PRESETS: [Preset; 3] = [Preset::Human, Preset::Table, Preset::Json];

/// The layers behind each row: `out[i]` for `rows[i]`, the stack a read of
/// that leaf consults, in its order, or `None` for a facet row or a row whose
/// field is not a leaf of the answer (a group shown at `--depth`). A row on
/// the caller's directory chain (its own directory, an ancestor, the global
/// entry) is read from the caller's directory, as `comb get` there would; a
/// row stored anywhere else is read from its own. One `layers` call per
/// provider and directory.
fn layer_stacks(
    rows: &[CacheRow],
    cwd: &str,
    session: &mut crate::client::ClientSession,
) -> Vec<Option<Vec<crate::cache::LayerRecord>>> {
    let canonical = std::fs::canonicalize(cwd)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| cwd.to_string());
    let chain = crate::query::path_chain(Some(&canonical));
    let mut trees: HashMap<(String, Option<String>), Option<serde_json::Value>> = HashMap::new();
    rows.iter()
        .map(|row| {
            if row_facet(&row.provider).is_some() {
                return None;
            }
            let at = if chain.contains(&row.path) {
                Some(canonical.clone())
            } else {
                row.path.clone()
            };
            let provider = row_base(&row.provider).to_string();
            let tree = trees
                .entry((provider.clone(), at.clone()))
                .or_insert_with(|| session.layers(&provider, at.as_deref()).ok().flatten());
            let mut node = tree.as_ref();
            for segment in libbeachcomber::key::segments(&row.field) {
                node = node.and_then(|n| n.get(segment.as_ref()));
            }
            match node {
                Some(list @ serde_json::Value::Array(_)) => {
                    Some(serde_json::from_value(list.clone()).unwrap_or_default())
                }
                _ => None,
            }
        })
        .collect()
}

/// Mark every row the read from `cwd` does not take: a record on the caller's
/// chain whose leaf another layer answers, or that the read answers nothing
/// at — a put that replaced the node above it, say. Such a row is not what
/// `comb get` here gives, so the default view hides it (`shadowed=false`).
/// A rendered row is the answer by definition, a facet row is no value, a row
/// stored off the chain is a read of its own, and a row whose field is not a
/// leaf has no stack to judge it by: none of those is marked.
fn mark_shadowed(
    rows: &mut [CacheRow],
    stacks: &[Option<Vec<crate::cache::LayerRecord>>],
    cwd: &str,
) {
    let canonical = std::fs::canonicalize(cwd)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| cwd.to_string());
    let chain = crate::query::path_chain(Some(&canonical));
    for (row, stack) in rows.iter_mut().zip(stacks) {
        let Some(stack) = stack else { continue };
        if matches!(row.kind, Some(RowKind::Template)) || !chain.contains(&row.path) {
            continue;
        }
        // A row's SOURCE and a layer's label are both the presented label.
        row.shadowed = !stack.iter().any(|l| {
            l.answered && l.facet.is_none() && l.path == row.path && l.label == row.source
        });
    }
}

/// Which record a row shows, to find its stack again after filtering and
/// sorting.
fn row_id(row: &CacheRow) -> (String, Option<String>, String, String) {
    (
        row.provider.clone(),
        row.path.clone(),
        row.field.clone(),
        row.source.clone(),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn run_status(
    config: &Config,
    format: &Format,
    user_filters: &[String],
    all: bool,
    sort_col: &str,
    no_trunc: bool,
    max_width_arg: Option<&str>,
    color_arg: &str,
    ascii: bool,
    depth: Option<usize>,
    tree: bool,
) -> ExitCode {
    if tree && !format.preset().is_some_and(|p| TREE_PRESETS.contains(&p)) {
        let named = format.preset().map_or("a template", Preset::name);
        eprintln!("error: --tree works with the human, table and json formats, not {named}");
        return ExitCode::from(2);
    }
    // A layer belongs to a leaf, so a tree destructures every value to its
    // leaves unless --depth says otherwise.
    let depth = depth.or(tree.then_some(0));
    let filters = &crate::cli::status_format::effective_filters(user_filters, all);
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
            if let Ok(cwd) = std::env::current_dir()
                && let Ok(mut session) = client.connect()
            {
                let cwd = cwd.to_string_lossy().into_owned();
                let rendered = render_template_rows(
                    &rows,
                    &cwd,
                    &crate::cli::shell_env(),
                    &mut session,
                    filters,
                );
                // A render reads what it needs, and a cold read runs the
                // Source behind it: the table is read again, so what this
                // render ran is a row of this status, not only the next.
                if !rendered.is_empty()
                    && let Ok(after) = client.status()
                {
                    rows = after;
                }
                rows.extend(rendered);
            }

            // Destructure before filtering and sorting, so both operate on the
            // field names the user can actually see in the output.
            let mut rows = flatten_rows(rows, opts.depth.unwrap_or_else(|| default_depth(format)));
            // The layers behind each row decide which rows a read here takes;
            // the rest are marked shadowed, which the default filters hide.
            let stacks: Option<HashMap<_, _>> = match (std::env::current_dir(), client.connect()) {
                (Ok(cwd), Ok(mut session)) => {
                    let cwd = cwd.to_string_lossy().into_owned();
                    let stacks = layer_stacks(&rows, &cwd, &mut session);
                    mark_shadowed(&mut rows, &stacks, &cwd);
                    Some(
                        rows.iter()
                            .zip(stacks)
                            .map(|(row, stack)| (row_id(row), stack.unwrap_or_default()))
                            .collect(),
                    )
                }
                (_, Err(e)) if tree => {
                    eprintln!("Error: {e}");
                    return ExitCode::from(2);
                }
                _ => None,
            };
            let rows = apply_filters(rows, filters).unwrap_or_else(|e| {
                eprintln!("filter error: {e}");
                std::process::exit(2);
            });
            let rows = apply_sort(rows, sort_col).unwrap_or_else(|e| {
                eprintln!("sort error: {e}");
                std::process::exit(2);
            });
            let out = match &stacks {
                Some(stacks) if tree => {
                    let children: Vec<_> = rows
                        .iter()
                        .map(|row| stacks.get(&row_id(row)).cloned().unwrap_or_default())
                        .collect();
                    render_tree(format, &rows, &children, &opts)
                }
                _ => render_preset(format, &rows, &opts),
            };
            print!("{out}");

            // Canon provider_source.md invariant 16 and singleton.md
            // invariant 12: watch and reaper
            // degradation are observable via `comb status`. Human
            // preset only, and on stderr so machine formats stay
            // parseable.
            if format == &Format::Preset(Preset::Human)
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
