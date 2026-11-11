//! Handler for the `status` (`s`) subcommand.

use crate::cache::{CacheRow, RowKind};
use crate::cli::exit::Outcome;
use crate::cli::output_format::{Format, Preset};
use crate::cli::status_format::{
    RenderOpts, apply_filters, apply_sort, default_depth, flatten_rows, render_preset,
    render_template_format, render_tree, resolve_max_width, row_base, row_facet,
};
use crate::cli::style::Style;
use crate::config::Config;
use libbeachcomber::resolve::{MemoWrite, Slot, eval_with, resolve_traced, write_back};
use std::collections::HashMap;
use std::io::IsTerminal;

/// The namespace facet holding value expressions.
const TEMPLATE_FACET: &str = ":template";

/// The SOURCE a rendered row reports: nothing produced it but this
/// resolution, here.
const RENDERED_SOURCE: &str = "rendered";

/// The SOURCE of a row whose leaf was served from its memo (`| cache()`).
const MEMO_SOURCE: &str = "memo";

/// One rendered row per template leaf the caller's directory can see.
///
/// The daemon holds templates as data and never evaluates them, so `status` shows raw expressions
/// where a user wants values. Each leaf under a `<p>:template` row visible
/// from `cwd` — the row's own path, an ancestor of it up to `$HOME`, or the
/// global entry — is resolved as `<p>.<leaf>` would be by
/// `comb get`, with `_.exec()` on: `comb status` is one of the surfaces that
/// opted in.
///
/// A leaf that resolves to nothing is a row with a null value, which the
/// default `value!=-` filter hides. A leaf that fails carries its failure as
/// its value — the cell is where the user looks for it, and a broken
/// expression is exactly what this view exists to show. A leaf served from
/// its memo says SOURCE `memo`, and is as old as the memo.
///
/// The memos the leaves rendered go onto `memos`, for the caller to write
/// back once the table is out.
///
/// `session` renders the first provider's leaves; other providers render at
/// once, each on a session of its own from `client`.
fn render_template_rows(
    rows: &[CacheRow],
    cwd: &str,
    env: &HashMap<String, String>,
    session: libbeachcomber::Session,
    client: &crate::client::Client,
    filters: &[String],
    memos: &mut Vec<MemoWrite>,
) -> Vec<CacheRow> {
    let chain = address_chain(cwd);
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

    let inputs = crate::cli::inputs(cwd, env, Slot::Cwd, false, libbeachcomber::Wait::Budget);
    // One provider's leaves run in order on one session, so they share its
    // memo hits and nothing runs twice; providers run side by side, so a
    // slow tool in one does not hold up the rest. Each group carries the
    // index of its first leaf in `addrs`; the queue pops from the back.
    let mut start = 0;
    let mut groups: Vec<Group<'_>> = addrs
        .chunk_by(|a, b| a.0 == b.0)
        .map(|group| {
            start += group.len();
            (start - group.len(), group)
        })
        .collect();
    groups.reverse();
    let queue = std::sync::Mutex::new(groups);
    let first = std::sync::Mutex::new(Some(session));
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get());
    let done: Vec<Vec<(usize, CacheRow, Vec<MemoWrite>)>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers.min(queue.lock().unwrap().len()))
            .map(|_| scope.spawn(|| render_groups(&queue, &first, client, &inputs, cwd)))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    // Back in address order, as one session rendering them in turn left them.
    let mut rendered: Vec<_> = done.into_iter().flatten().collect();
    rendered.sort_by_key(|(i, ..)| *i);
    rendered
        .into_iter()
        .map(|(_, row, written)| {
            memos.extend(written);
            row
        })
        .collect()
}

/// The next group off `queue`, its lock released on return.
fn pop<'a>(queue: &std::sync::Mutex<Vec<Group<'a>>>) -> Option<Group<'a>> {
    queue.lock().unwrap().pop()
}

/// One provider's leaves, `(provider, field)`, with the index of the first
/// in the full address list.
type Group<'a> = (usize, &'a [(String, String)]);

/// A worker of [`render_template_rows`]: takes provider groups off `queue`
/// until none are left, rendering each on one session — `first`'s if still
/// unclaimed, else its own. Each row comes back with its index in `addrs` and
/// the memos it rendered.
fn render_groups(
    queue: &std::sync::Mutex<Vec<Group<'_>>>,
    first: &std::sync::Mutex<Option<libbeachcomber::Session>>,
    client: &crate::client::Client,
    inputs: &libbeachcomber::resolve::Inputs<'_>,
    cwd: &str,
) -> Vec<(usize, CacheRow, Vec<MemoWrite>)> {
    let mut session = None;
    let mut out = Vec::new();
    // Popped through `pop`: a `while let` on the lock itself would hold it
    // for the whole loop body, and the workers would take turns.
    while let Some((start, group)) = pop(queue) {
        let session = session.get_or_insert_with(|| {
            first
                .lock()
                .unwrap()
                .take()
                .map_or_else(|| client.connect(), Ok)
        });
        for (i, (provider, field)) in group.iter().enumerate() {
            let mut written = Vec::new();
            // A rendered value has no age of its own: it is as old as the
            // oldest cached input it used, and has none when it used none.
            let (value, age_ms, source) = match session
                .as_mut()
                .map_err(|e| e.to_string())
                .and_then(|session| {
                    resolve_traced(&libbeachcomber::key::join(provider, field), inputs, session)
                        .map_err(|e| e.to_string())
                }) {
                Ok(traced) => {
                    written = traced.memos;
                    (
                        traced.value.unwrap_or(serde_json::Value::Null),
                        traced.oldest_input_ms,
                        if traced.memo_hit {
                            MEMO_SOURCE
                        } else {
                            RENDERED_SOURCE
                        },
                    )
                }
                Err(e) => (serde_json::Value::String(e), None, RENDERED_SOURCE),
            };
            out.push((
                start + i,
                CacheRow {
                    warn: None,
                    shadowed: false,
                    provider: provider.clone(),
                    path: Some(cwd.to_string()),
                    source: source.to_string(),
                    field: field.clone(),
                    value,
                    age_ms,
                    kind: Some(RowKind::Rendered),
                    poll_interval_secs: None,
                    poll_count: None,
                    polls_elapsed: None,
                    next_poll_in_secs: None,
                    failure: None,
                },
                written,
            ));
        }
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
    session: &mut libbeachcomber::Session,
) -> Vec<Option<Vec<crate::cache::LayerRecord>>> {
    let canonical = std::fs::canonicalize(cwd)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| cwd.to_string());
    let chain = address_chain(&canonical);
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
    let chain = address_chain(&canonical);
    for (row, stack) in rows.iter_mut().zip(stacks) {
        let Some(stack) = stack else { continue };
        if matches!(row.kind, Some(RowKind::Rendered)) || !chain.contains(&row.path) {
            continue;
        }
        // A row's SOURCE and a layer's label are both the presented label.
        row.shadowed = !stack.iter().any(|l| {
            l.answered && l.facet.is_none() && l.path == row.path && l.label == row.source
        });
    }
}

/// A row's provider, path, field and source.
type RowId = (String, Option<String>, String, String);

/// Which record a row shows, to find its stack again after filtering and
/// sorting.
fn row_id(row: &CacheRow) -> RowId {
    (
        row.provider.clone(),
        row.path.clone(),
        row.field.clone(),
        row.source.clone(),
    )
}

/// The layers behind each row, by [`row_id`].
type Stacks = HashMap<RowId, Vec<crate::cache::LayerRecord>>;

#[allow(clippy::too_many_arguments)]
pub fn run_status(
    config: &Config,
    format: &Format,
    user_filters: &[String],
    all: bool,
    sort_col: &str,
    no_trunc: bool,
    max_width_arg: Option<&str>,
    style: Style,
    depth: Option<usize>,
    tree: bool,
) -> Outcome {
    match status(
        config,
        format,
        user_filters,
        all,
        sort_col,
        no_trunc,
        max_width_arg,
        style,
        depth,
        tree,
    ) {
        Ok(()) => Outcome::Done,
        Err(outcome) => outcome,
    }
}

#[allow(clippy::too_many_arguments)]
fn status(
    config: &Config,
    format: &Format,
    user_filters: &[String],
    all: bool,
    sort_col: &str,
    no_trunc: bool,
    max_width_arg: Option<&str>,
    style: Style,
    depth: Option<usize>,
    tree: bool,
) -> Result<(), Outcome> {
    check_tree_format(format, tree)?;
    // A layer belongs to a leaf, so a tree destructures every value to its
    // leaves unless --depth says otherwise.
    let depth = depth.or(tree.then_some(0));
    let filters = &crate::cli::status_format::effective_filters(user_filters, all);
    // A typo costs nothing: rendering every row can run commands.
    apply_filters(Vec::new(), filters).map_err(|e| flag_error("--filter", e))?;
    apply_sort(Vec::new(), sort_col).map_err(|e| flag_error("--sort", e))?;
    let cols = terminal_size::terminal_size().map(|(w, _)| w.0 as usize);
    let max_width =
        resolve_max_width(max_width_arg, cols).map_err(|e| flag_error("--max-width", e))?;
    let socket_path = crate::cli::connect::connect_as_is(config)?;
    warn_if_daemon_older(&socket_path);

    let Style { color, ascii } = style;
    let opts = RenderOpts {
        is_tty: std::io::stdout().is_terminal(),
        no_color: !color,
        max_width: if no_trunc { None } else { Some(max_width) },
        no_trunc,
        ascii,
        depth,
    };

    let client = crate::client::Client::new(socket_path);
    // The memos the rendered rows' cached templates rendered, written back
    // once the table is printed.
    let mut memos = Vec::new();
    let rows = client.status().map_err(daemon_down)?;
    let rows = with_rendered_rows(&client, rows, filters, &mut memos);

    // Destructure before filtering and sorting, so both operate on the
    // field names the user can actually see in the output.
    let mut rows = flatten_rows(rows, opts.depth.unwrap_or_else(|| default_depth(format)));
    let stacks = mark_shadowed_rows(&client, &mut rows, tree)?;
    let rows = apply_filters(rows, filters).map_err(|e| flag_error("--filter", e))?;
    let rows = apply_sort(rows, sort_col).map_err(|e| flag_error("--sort", e))?;

    let out = render_rows(&client, format, &rows, stacks.as_ref(), tree, &opts)?;
    print!("{out}");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    if !memos.is_empty()
        && let Ok(mut session) = client.connect()
    {
        write_back(&memos, &mut session);
    }
    // Human preset only, and on stderr so machine formats stay parseable.
    if format == &Format::Preset(Preset::Human) {
        warn_if_daemon_degraded(&client);
        for warning in source_warnings(&rows) {
            crate::cli::report::warning(warning);
        }
    }
    Ok(())
}

/// Each Source instance's latest warning among `rows`, once: `<provider>
/// <source> at <path>: <msg>`.
fn source_warnings(rows: &[CacheRow]) -> Vec<String> {
    let home = libbeachcomber::path::Home::from_env();
    let mut out: Vec<String> = rows
        .iter()
        .filter_map(|r| {
            let at = r
                .path
                .as_deref()
                .map_or_else(|| "~".into(), |p| home.compact(p));
            Some(format!(
                "{} {} at {at}: {}",
                r.provider,
                r.source,
                r.warn.as_ref()?
            ))
        })
        .collect();
    let mut seen = std::collections::HashSet::new();
    out.retain(|w| seen.insert(w.clone()));
    out
}

/// `--tree` draws the human, table and json formats only.
fn check_tree_format(format: &Format, tree: bool) -> Result<(), Outcome> {
    if tree && !format.preset().is_some_and(|p| TREE_PRESETS.contains(&p)) {
        let named = format.preset().map_or("a template", Preset::name);
        crate::cli::report::error(
            "--tree",
            format_args!("works with the human, table and json formats, not {named}"),
        );
        return Err(Outcome::Error);
    }
    Ok(())
}

/// Report a bad flag value and yield the failing outcome.
fn flag_error(flag: &str, message: String) -> Outcome {
    crate::cli::report::error(flag, message);
    Outcome::Error
}

/// Report an unreachable daemon and yield the failing outcome.
fn daemon_down(e: impl std::fmt::Display) -> Outcome {
    crate::cli::report::daemon_unreachable(e);
    Outcome::Error
}

/// Shown, not fixed: `status` reports the daemon it finds.
fn warn_if_daemon_older(socket_path: &std::path::Path) {
    if let Ok(hello) = crate::client::build_client(socket_path).hello()
        && crate::cli::connect::older(&hello.daemon_version, libbeachcomber::VERSION)
    {
        crate::cli::report::warning(format_args!(
            "daemon {} is older than comb {}; the next query restarts it",
            hello.daemon_version,
            libbeachcomber::VERSION
        ));
    }
}

/// The daemon's rows plus the rendered rows.
///
/// The daemon never evaluates a template, so the rendered rows are added
/// here, from this shell's directory and environment. A template resolves
/// relative to `cwd`; with no cwd to resolve against,
/// there is no correct directory to stamp a rendered row with, so rendering
/// is skipped rather than resolving every leaf as if the caller stood at ""
/// — which stamped rows with `path: Some("")`, an empty PATH cell that reads
/// as a lie. The daemon's own rows still print.
fn with_rendered_rows(
    client: &crate::client::Client,
    mut rows: Vec<CacheRow>,
    filters: &[String],
    memos: &mut Vec<MemoWrite>,
) -> Vec<CacheRow> {
    if let Ok(cwd) = std::env::current_dir()
        && let Ok(session) = client.connect()
    {
        let cwd = cwd.to_string_lossy().into_owned();
        let rendered = render_template_rows(
            &rows,
            &cwd,
            &crate::cli::shell_env(),
            session,
            client,
            filters,
            memos,
        );
        // A render reads what it needs, and a cold read runs the Source
        // behind it: the table is read again, so what this render ran is a
        // row of this status, not only the next.
        if !rendered.is_empty()
            && let Ok(after) = client.status()
        {
            rows = after;
        }
        rows.extend(rendered);
    }
    rows
}

/// Mark the rows a read here does not take as shadowed (the default filters
/// hide them), and return the layers behind each row, by [`row_id`]. The
/// layers decide which rows a read here takes. `None` when there is no cwd or
/// no daemon session to read them with; that is an error only under `--tree`,
/// which needs them.
fn mark_shadowed_rows(
    client: &crate::client::Client,
    rows: &mut [CacheRow],
    tree: bool,
) -> Result<Option<Stacks>, Outcome> {
    match (std::env::current_dir(), client.connect()) {
        (Ok(cwd), Ok(mut session)) => {
            let cwd = cwd.to_string_lossy().into_owned();
            let stacks = layer_stacks(rows, &cwd, &mut session);
            mark_shadowed(rows, &stacks, &cwd);
            Ok(Some(
                rows.iter()
                    .zip(stacks)
                    .map(|(row, stack)| (row_id(row), stack.unwrap_or_default()))
                    .collect(),
            ))
        }
        (_, Err(e)) if tree => Err(daemon_down(e)),
        _ => Ok(None),
    }
}

/// The output text: a `-f` template per row, a `--tree`, or a preset.
fn render_rows(
    client: &crate::client::Client,
    format: &Format,
    rows: &[CacheRow],
    stacks: Option<&Stacks>,
    tree: bool,
    opts: &RenderOpts,
) -> Result<String, Outcome> {
    match (stacks, format) {
        (_, Format::Template(template)) if !tree => {
            let cwd = std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let env = crate::cli::shell_env();
            let inputs =
                crate::cli::inputs(&cwd, &env, Slot::Cwd, false, libbeachcomber::Wait::Budget);
            let mut session = client.connect().map_err(daemon_down)?;
            let mut render = |src: &str, locals: &serde_json::Map<_, _>| {
                eval_with(src, "-f", &inputs, &mut session, locals)
            };
            render_template_format(template, rows, &mut render).map_err(|e| {
                crate::cli::report::error_named(e);
                Outcome::Error
            })
        }
        (Some(stacks), _) if tree => {
            let children: Vec<_> = rows
                .iter()
                .map(|row| stacks.get(&row_id(row)).cloned().unwrap_or_default())
                .collect();
            Ok(render_tree(format, rows, &children, opts))
        }
        _ => Ok(render_preset(format, rows, opts)),
    }
}

/// Canon provider_source.md invariant 16 and singleton.md invariant 12:
/// watch and reaper degradation are observable via `comb status`.
fn warn_if_daemon_degraded(client: &crate::client::Client) {
    let Ok(resp) = client.introspect("daemon", None) else {
        return;
    };
    let Some(data) = resp.data.as_ref().filter(|_| resp.ok) else {
        return;
    };
    if let Some(backend) = data.get("watch_backend").and_then(|v| v.as_str())
        && backend != "native"
    {
        crate::cli::report::warning(format_args!(
            "watch backend: {backend} — kernel fs events undelivered; watch invalidation degraded"
        ));
    }
    if let Some(reaper) = data.get("reaper").filter(|r| !r.is_null()) {
        let armed = reaper
            .get("armed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let confined = reaper.get("visibility").and_then(|v| v.as_str()) == Some("confined");
        let denied = reaper
            .get("kill_denied")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if armed && confined {
            crate::cli::report::warning(
                "reaper visibility degraded: daemon runs confined — orphan daemons outside its view are not policed",
            );
        }
        if armed && denied > 0 {
            crate::cli::report::warning(format_args!(
                "reaper: {denied} kill attempt(s) denied by the OS"
            ));
        }
    }
}

/// The cache addresses a read at `dir` consults, nearest first
/// ([`crate::query::Dir::chain`]), against this process's `$HOME`.
fn address_chain(dir: &str) -> Vec<Option<String>> {
    let home = crate::query::Home::from_env();
    crate::query::Dir::of(dir, &home)
        .chain(&home)
        .into_iter()
        .map(crate::query::Dir::into_address)
        .collect()
}
