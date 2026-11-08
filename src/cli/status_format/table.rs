//! The `human` and `table` presets: rows laid out as aligned columns under a
//! header, fitted to a total width.
//!
//! One render is a pipeline: [`measure`] every column's natural width,
//! [`fit`] the flexible columns to the width cap, build a line of
//! [`Cell`]s per row, then [`layout`] the lines with margins and stripes.

use super::ansi;
use super::cells::{
    MARKER_WIDTH, TtlCell, TtlWidths, colour_value, compute_ttl_widths, format_age_with_polls,
    format_poll_cell, format_ttl_cell, row_is_tainted, sanitize_cell, template_marker,
};
use super::row::{ChildMark, shown_path, shown_value, tree_display_rows};
use super::text::{Cols, ellipsis, truncate, truncate_middle, truncate_path};
use super::{FormatOptions, RenderOpts};
use crate::cache::CacheRow;
use crate::cli::output_format::{Format, Preset};
use crate::cli::style::Glyphs;
use libbeachcomber::path::Home;
use libbeachcomber::term;

// Column indices for the human/table preset.
const COLS: usize = 8;
// TTL is the last column (index 7); failure-state colouring skips only this
// one (its own warn-glyph + red already come from format_ttl_cell). POLL
// (index 6) is a plain column and gets red-wrapped like the others.
const TTL_COL_IDX: usize = 7;
// PROVIDER and FIELD sit adjacent because together they are the cache key the
// user typed (`git.branch`); SOURCE follows, naming what produced the VALUE
// beside it (a Source, `put`, `config(<file>)`, or `rendered` for a row this
// CLI rendered); PATH follows VALUE as the scope qualifier rather than
// splitting the key. Human/table presets only — the machine presets
// (json/tsv/csv/sh) keep their own established column order.
const HEADERS: [&str; COLS] = [
    "PROVIDER", "FIELD", "SOURCE", "VALUE", "PATH", "AGE", "POLL", "TTL",
];
const H_AGE: usize = 5;
const H_POLL: usize = 6;

/// Ceiling on the SOURCE column in the human preset, lifted by
/// `--no-trunc`.
///
/// SOURCE would otherwise take its natural width like PROVIDER, because a
/// Source name is short (`refs`, `head`, `put`, `rendered`). One kind is not:
/// a `config:<file>` label is a whole path, and uncapped it pinned VALUE and
/// PATH at their floors across every row of the table. Its tail
/// (`…/defaults/aws.toml`) is what tells one config file from another, so the
/// cell truncates from the head, like PATH does.
const MAX_SOURCE_WIDTH: usize = 24;

/// What the human and table presets show for an absent value: a template leaf
/// that rendered nothing (shown with `--all` or `--filter value=-`).
const ABSENT_TEXT: &str = "null";

// Under `watch`, AGE and POLL tick every refresh ("9s" → "12s" →
// "1m16s×1"), and a column sized to the snapshot's widest value shifts
// every boundary to its right as that value changes width. Floor both at
// a width covering the common forms and right-align the cells (POLL
// already is), so the digits stay put between refreshes. The floors are
// given back if a tight cap can't afford them ([`fit`]).
const AGE_MIN_WIDTH: usize = 8; // "3m54s×0"
const POLL_MIN_WIDTH: usize = 4;

const MIN_VALUE_WIDTH: usize = 8;
// Wide enough that a tail-truncated session path (`…/project#session-id`)
// or destructured field name (`…incident_updates.0.new_status`) still
// shows the leaf that discriminates it.
const MIN_PATH_WIDTH: usize = 20;
const MIN_FIELD_WIDTH: usize = 20;
const SEP_TOTAL: usize = 14; // 7 separators × 2 spaces
const MARGIN: usize = 2; // left margin, printed; right margin, reserved only
const MARGIN_TOTAL: usize = MARGIN * 2;

/// `comb status --tree`: each value row followed by the layers behind it
/// (`children[i]` for `rows[i]`), in the `human`, `table` or `json` preset.
/// `json` gives each row its whole `layers` array; the tables draw each layer
/// as a row of its own under its value, except the one that is the row
/// itself, so a value with nothing behind it is drawn alone.
pub fn render_tree(
    format: &Format,
    rows: &[CacheRow],
    children: &[Vec<crate::cache::LayerRecord>],
    opts: &RenderOpts,
) -> String {
    if format == &Format::Preset(Preset::Json) {
        let mut out = String::new();
        for (row, layers) in rows.iter().zip(children) {
            if let Ok(mut v) = serde_json::to_value(row) {
                v["layers"] = serde_json::to_value(layers).unwrap_or_default();
                out.push_str(&v.to_string());
                out.push('\n');
            }
        }
        return out;
    }
    let (display, marks) = tree_display_rows(rows, children);
    match format {
        Format::Preset(Preset::Table) => render_table(&display, &marks, false, None, false),
        _ => {
            let trunc = if opts.no_trunc { None } else { opts.max_width };
            render_table(&display, &marks, !opts.no_color, trunc, opts.ascii)
        }
    }
}

/// Render rows as the plain aligned table: no colour, no width cap, a header.
/// `opts.ascii` picks ASCII-only glyphs over Unicode ones.
pub fn render_human(rows: &[CacheRow], opts: &FormatOptions) -> String {
    render_table(rows, &[], false, None, opts.ascii)
}

/// Render rows as aligned columns under a PROVIDER / FIELD / SOURCE / VALUE /
/// PATH / AGE / POLL / TTL header.
///
/// - `marks`: parallel to `rows`; `Some` draws that row as a `--tree` layer.
/// - `color_enabled`: apply per-cell ANSI colour (AGE yellow when its last run
///   failed, else green, TTL by decay, failure rows red on non-TTL cells, JSON
///   syntax highlighting in structured VALUE cells).
/// - `trunc`: the **total table width cap** (all columns + padding); the
///   flexible columns shrink to fit it. `None` = no truncation.
/// - `ascii`: use ASCII-only glyphs instead of Unicode.
pub(super) fn render_table(
    rows: &[CacheRow],
    marks: &[Option<ChildMark>],
    color_enabled: bool,
    trunc: Option<usize>,
    ascii: bool,
) -> String {
    let env = Env {
        color: color_enabled,
        ascii,
        // Display-only: a path under $HOME is written from `~`, and the
        // global entry is `~` itself. Machine presets never come through
        // here — they use `row.path` directly.
        home: Home::from_env(),
        now_unix_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        // Auto-size the TTL cell's `{P}s×{KK} (total)` sub-fields to the
        // widest values in this snapshot.
        ttl_widths: compute_ttl_widths(rows),
        // Budget-aware: SOURCE competes with VALUE/PATH for the same `trunc`
        // total, so a 24-char config label alone must not starve those
        // columns at a narrow cap.
        source_cap: trunc.map(|c| MAX_SOURCE_WIDTH.min(c / 5)),
    };
    let mark_of = |index: usize| marks.get(index).copied().flatten();

    let mut widths = measure(rows, &mark_of, &env);
    let caps = match trunc {
        Some(total_cap) => fit(&mut widths, total_cap),
        None => Caps::default(),
    };

    let mut lines = vec![header_line(&widths, color_enabled)];
    for (index, row) in rows.iter().enumerate() {
        lines.push(data_line(row, mark_of(index), &env, &widths, &caps));
    }
    layout(&lines, color_enabled)
}

/// What every row of one render shares.
struct Env {
    color: bool,
    ascii: bool,
    home: Home,
    now_unix_ms: u64,
    ttl_widths: TtlWidths,
    source_cap: Option<usize>,
}

/// One table cell: its text, possibly carrying ANSI escapes, and the visible
/// width alignment counts (without them).
struct Cell {
    text: String,
    vis: usize,
}

impl Cell {
    fn plain(text: String) -> Self {
        let vis = text.cols();
        Cell { text, vis }
    }
}

// ---------------------------------------------------------------------------
// Natural widths
// ---------------------------------------------------------------------------

/// Every column's natural width: its widest cell, header included, before any
/// truncation. `age_raw` and `poll_raw` are AGE's and POLL's before the
/// stability floors.
struct Widths {
    provider: usize,
    field: usize,
    source: usize,
    value: usize,
    path: usize,
    age: usize,
    poll: usize,
    ttl: usize,
    age_raw: usize,
    poll_raw: usize,
}

/// How much of a failing row's error its VALUE cell shows, before the width
/// cap: the machine formats carry the whole message.
const LAST_ERROR_CHARS: usize = 160;

/// `message`'s first line, cut to [`LAST_ERROR_CHARS`] characters with an
/// ellipsis when longer.
fn first_line(message: &str) -> String {
    let line = message.lines().next().unwrap_or_default().trim_end();
    if line.chars().count() <= LAST_ERROR_CHARS {
        return line.to_string();
    }
    let mut cut: String = line.chars().take(LAST_ERROR_CHARS - 1).collect();
    cut.push('\u{2026}');
    cut
}

/// A row's PATH and VALUE as shown, before sanitizing, and whether any
/// provider-controlled text in it needs sanitizing (and so gets a marker).
fn row_texts(row: &CacheRow, env: &Env) -> (String, String, bool) {
    let path = shown_path(row.path.as_deref(), &env.home);
    let value = match shown_value(row) {
        // Nothing stored: the VALUE cell is the latest error.
        Some(error) if row.value.is_null() => first_line(&error),
        shown => shown.unwrap_or_else(|| ABSENT_TEXT.to_string()),
    };
    let tainted = row_is_tainted(row, &path, &value);
    (path, value, tainted)
}

fn measure(rows: &[CacheRow], mark_of: &dyn Fn(usize) -> Option<ChildMark>, env: &Env) -> Widths {
    let mut w = Widths {
        provider: HEADERS[0].len(),
        field: HEADERS[1].len(),
        source: HEADERS[2].len(),
        value: HEADERS[3].len(),
        path: HEADERS[4].len(),
        age: HEADERS[H_AGE].len(),
        poll: HEADERS[H_POLL].len(),
        ttl: HEADERS[TTL_COL_IDX].len(),
        age_raw: 0,
        poll_raw: 0,
    };
    for (index, row) in rows.iter().enumerate() {
        let (path, value, tainted) = row_texts(row, env);
        // Sanitizing swaps one char for one char, so it never changes a
        // natural width. Only the taint marker does.
        let taint_width = if tainted { MARKER_WIDTH } else { 0 };
        w.path = w.path.max(path.cols());
        w.source = w.source.max(match env.source_cap {
            Some(cap) => row.source.cols().min(cap),
            None => row.source.cols(),
        });
        if let Some(mark) = mark_of(index) {
            // A layer row has no PROVIDER, POLL or TTL of its own.
            w.field = w.field.max(mark.field(env.ascii).cols());
            w.value = w.value.max(value.cols() + taint_width);
            w.age = w
                .age
                .max(format_age_with_polls(row.age_ms, None, env.ascii).cols());
            continue;
        }
        let marker_width =
            taint_width + template_marker(row.kind.as_ref(), env.color, env.ascii).cols();
        w.provider = w.provider.max(row.provider.cols());
        w.field = w.field.max(row.field.cols());
        w.value = w.value.max(value.cols() + marker_width);
        w.age = w
            .age
            .max(format_age_with_polls(row.age_ms, row.polls_elapsed, env.ascii).cols());
        w.ttl = w.ttl.max(ttl_cell_of(row, env).text.cols());
        w.poll = w.poll.max(poll_text_of(row, env).cols());
    }
    w.age_raw = w.age;
    w.poll_raw = w.poll;
    w.age = w.age.max(AGE_MIN_WIDTH);
    w.poll = w.poll.max(POLL_MIN_WIDTH);
    w
}

// ---------------------------------------------------------------------------
// Fitting to the width cap
// ---------------------------------------------------------------------------

/// The widths the flexible columns are cut to; `None` leaves a column at its
/// natural width.
#[derive(Default)]
struct Caps {
    field: Option<usize>,
    path: Option<usize>,
    value: Option<usize>,
}

/// Split the total-table-width budget across the flexible columns.
///
/// Layout: MARGIN provider(2sp)field(2sp)source(2sp)value(2sp)path(2sp)
/// age(2sp)poll(2sp)ttl MARGIN. Separators: 7 × 2 = 14. Margins: 2 spaces
/// each side (breathing room so the table doesn't hug the terminal edge).
///
/// VALUE, FIELD and PATH are the three flexible columns; PROVIDER, SOURCE,
/// AGE, POLL and TTL always take their natural widths. Whatever the budget
/// leaves after those five is `flex`.
///
/// All three are unbounded in practice — values are arbitrary JSON, paths
/// carry `#session` suffixes, and destructuring makes field names as deep
/// as the data. Any one of them left uncapped pins the other two at their
/// floors across the whole table: that is what an uncapped PATH did to
/// VALUE before, and what an uncapped FIELD did once destructuring landed.
///
/// Split: FIELD first, natural up to a flex-relative ceiling — losing a
/// few chars of a field name hurts more than losing them from a JSON
/// blob, so a modest name (shallow destructuring) eats from VALUE; the
/// ceiling keeps a deep name (75+ chars at --depth 0) from starving
/// VALUE outright. VALUE then takes what remains above PATH's floor;
/// PATH takes what both decline.
fn fit(w: &mut Widths, total_cap: usize) -> Caps {
    let fixed =
        |w: &Widths| w.provider + w.source + w.age + w.poll + w.ttl + SEP_TOTAL + MARGIN_TOTAL;
    let mut flex = total_cap.saturating_sub(fixed(w));
    // A tight cap may not afford the AGE/POLL stability floors: if the
    // flexible columns can't stand on their own floors, give the padding
    // back — a stable column is pointless when the row overflows the cap.
    let flex_floors =
        MIN_FIELD_WIDTH.min(w.field) + MIN_PATH_WIDTH.min(w.path) + MIN_VALUE_WIDTH.min(w.value);
    if flex < flex_floors && (w.age > w.age_raw || w.poll > w.poll_raw) {
        w.age = w.age_raw;
        w.poll = w.poll_raw;
        flex = total_cap.saturating_sub(fixed(w));
    }
    if w.value + w.field + w.path <= flex {
        // All three fit at their natural widths: nothing truncates.
        return Caps::default();
    }
    // A floor never widens a column past what it actually needs, hence the
    // `.min(w.*)` on each.
    let field_floor = MIN_FIELD_WIDTH.min(w.field);
    let path_floor = MIN_PATH_WIDTH.min(w.path);

    let field_w = w.field.min((flex * 2 / 5).max(field_floor));
    let value_w = w
        .value
        .min(flex.saturating_sub(field_w + path_floor))
        .max(MIN_VALUE_WIDTH.min(w.value));
    let path_w = w
        .path
        .min(flex.saturating_sub(value_w + field_w))
        .max(path_floor);

    Caps {
        field: (field_w < w.field).then_some(field_w),
        path: (path_w < w.path).then_some(path_w),
        value: (value_w < w.value).then_some(value_w),
    }
}

// ---------------------------------------------------------------------------
// Cells
// ---------------------------------------------------------------------------

/// The header line. AGE and POLL cells are right-aligned; their headers sit
/// over the digits by pre-padding them to the same width.
fn header_line(w: &Widths, color: bool) -> [Cell; COLS] {
    let mut texts: [String; COLS] = HEADERS.map(str::to_string);
    texts[H_AGE] = format!("{:>w$}", HEADERS[H_AGE], w = w.age);
    texts[H_POLL] = format!("{:>w$}", HEADERS[H_POLL], w = w.poll);
    texts.map(|h| Cell {
        vis: h.len(),
        text: ansi::wrap(&h, ansi::Color::Bold, color),
    })
}

/// `text` cut to `cap` columns by `cut`, when there is a cap.
fn capped(
    text: String,
    cap: Option<usize>,
    cut: fn(&str, usize, &str) -> String,
    ell: &str,
) -> String {
    match cap {
        Some(cap) => cut(&text, cap, ell),
        None => text,
    }
}

/// One data row's cells. Provider-controlled text is sanitized first, then
/// truncated — never the other way round, so a truncation can't leave half an
/// escape sequence intact.
fn data_line(
    row: &CacheRow,
    mark: Option<ChildMark>,
    env: &Env,
    w: &Widths,
    caps: &Caps,
) -> [Cell; COLS] {
    let ell = ellipsis(env.ascii);
    let (path_raw, value_raw, tainted) = row_texts(row, env);

    let provider = match mark {
        Some(_) => String::new(),
        None => sanitize_cell(&row.provider, env.ascii),
    };
    let field = match mark {
        Some(mark) => mark.field(env.ascii),
        None => sanitize_cell(&row.field, env.ascii),
    };
    let source = capped(
        sanitize_cell(&row.source, env.ascii),
        env.source_cap,
        truncate_path,
        ell,
    );
    let field = capped(field, caps.field, truncate_middle, ell);
    let path = capped(
        sanitize_cell(&path_raw, env.ascii),
        caps.path,
        truncate_path,
        ell,
    );

    let mut cells = [
        Cell::plain(provider),
        Cell::plain(field),
        Cell::plain(source),
        value_cell(row, &value_raw, tainted, env, caps.value),
        Cell::plain(path),
        age_cell(row, env, w.age),
        poll_cell(row, mark, env, w.poll),
        ttl_cell(row, mark, env),
    ];

    // Failure-state colouring: red on all cells except TTL (which already
    // has its own ⚠ + red from format_ttl_cell). POLL is a plain cell — it
    // gets red-wrapped like PROVIDER/PATH/FIELD/VALUE/AGE.
    if env.color && row.failure.is_some() {
        for (i, cell) in cells.iter_mut().enumerate() {
            if i != TTL_COL_IDX {
                cell.text = ansi::wrap(&cell.text, ansi::Color::Red, true);
            }
        }
    }
    // A layer the read did not take is dimmed, beside the `shadowed` its
    // FIELD cell says ([`ChildMark::field`]).
    if env.color && mark.is_some_and(|m| !m.answered) {
        for cell in cells.iter_mut() {
            cell.text = ansi::wrap(&term::strip_escapes(&cell.text), ansi::Color::Dim, true);
        }
    }
    cells
}

/// The VALUE cell. A sanitized row is flagged in it, so the eye can find the
/// rows whose text was rewritten without diffing against the source. The
/// markers are part of the cell, so they take part in the width budget and
/// truncate with everything else.
fn value_cell(
    row: &CacheRow,
    value_raw: &str,
    tainted: bool,
    env: &Env,
    cap: Option<usize>,
) -> Cell {
    let taint = if tainted {
        format!("{} ", Glyphs::of(env.ascii).tainted)
    } else {
        String::new()
    };
    // A rendered row is marked ahead of the taint marker: colour-off builds
    // need to see "this was computed here" before anything else in the cell
    // (design decision 5).
    let rendered = template_marker(row.kind.as_ref(), env.color, env.ascii);
    let value = format!("{rendered}{taint}{}", sanitize_cell(value_raw, env.ascii));
    let value = capped(value, cap, truncate, ellipsis(env.ascii));
    let vis = value.cols();
    let text = if env.color {
        colour_value(row, value, tainted)
    } else {
        value
    };
    Cell { text, vis }
}

/// The AGE cell, right-aligned against the floored column width (see
/// [`AGE_MIN_WIDTH`]), yellow when its last run failed.
fn age_cell(row: &CacheRow, env: &Env, width: usize) -> Cell {
    let text = format!(
        "{:>width$}",
        format_age_with_polls(row.age_ms, row.polls_elapsed, env.ascii)
    );
    let vis = text.cols();
    let text = if !env.color {
        text
    } else if row
        .failure
        .as_ref()
        .is_some_and(|f| f.consecutive_failures > 0)
    {
        ansi::wrap(&text, ansi::Color::Yellow, true)
    } else {
        ansi::wrap(&text, ansi::Color::BrightGreen, true)
    };
    Cell { text, vis }
}

/// A row's TTL cell, from its lifecycle metadata.
fn ttl_cell_of(row: &CacheRow, env: &Env) -> TtlCell {
    format_ttl_cell(
        row.kind.as_ref(),
        row.poll_interval_secs,
        row.poll_count,
        row.failure.as_ref(),
        env.ascii,
        &env.ttl_widths,
    )
}

/// The TTL cell. TTL is the last column, so the padding in [`layout`] (which
/// only pads non-last columns) never touches it; it has its own internal
/// fixed-width discipline via `ttl_widths` instead. A layer row has no
/// lifecycle of its own to show.
fn ttl_cell(row: &CacheRow, mark: Option<ChildMark>, env: &Env) -> Cell {
    let ttl = if mark.is_some() {
        TtlCell {
            text: String::new(),
            color: None,
        }
    } else {
        ttl_cell_of(row, env)
    };
    let vis = ttl.text.cols();
    let text = match (env.color, ttl.color) {
        (true, Some(c)) => ansi::wrap(&ttl.text, c, true),
        _ => ttl.text,
    };
    Cell { text, vis }
}

/// Seconds until next poll / next retry attempt, as a bare string.
fn poll_text_of(row: &CacheRow, env: &Env) -> String {
    format_poll_cell(
        row.kind.as_ref(),
        row.next_poll_in_secs,
        row.failure.as_ref(),
        env.now_unix_ms,
    )
}

/// The POLL cell. Unlike PROVIDER/PATH/FIELD/etc., POLL is right-aligned
/// (it's a number), so it is pre-padded here against its column width rather
/// than relying on the (left-justifying) padding in [`layout`].
fn poll_cell(row: &CacheRow, mark: Option<ChildMark>, env: &Env, width: usize) -> Cell {
    let text = if mark.is_some() {
        String::new()
    } else {
        poll_text_of(row, env)
    };
    Cell::plain(format!("{text:>width$}"))
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// Join lines (the header first) into the table: margin, two-space
/// separators, columns padded to their widest visible cell.
fn layout(lines: &[[Cell; COLS]], color: bool) -> String {
    let mut col_widths = [0usize; COLS];
    for line in lines {
        for (width, cell) in col_widths.iter_mut().zip(line) {
            *width = (*width).max(cell.vis);
        }
    }

    // The right margin is reserved in the width budget but never painted.
    // Emitting it padded every row out to exactly `max_width` — i.e. exactly
    // the terminal width — which viewers that reserve a column wrap: viddy
    // paints a scrollbar in the last column, so a full-width row spilled its
    // trailing space onto the next screen line and the whole table rendered
    // double-spaced. Reserving without printing keeps the two columns of
    // breathing room and leaves the rows free of trailing whitespace.
    let left_margin = " ".repeat(MARGIN);
    let stripe = ansi::stripe_bg();
    let mut out = String::new();
    for (idx, line) in lines.iter().enumerate() {
        // Alternate a subtle background on every other data row so the eye
        // can track across long lines. Colour-on only, so watch/viddy
        // without --color=always (and every machine consumer) see plain
        // rows. The header (line 0) is excluded from the count.
        let striped = color && idx >= 1 && (idx - 1) % 2 == 1;

        let mut text = String::new();
        for (i, cell) in line.iter().enumerate() {
            if i > 0 {
                text.push_str("  ");
            }
            text.push_str(&cell.text);
            // Pad to column width on all but the last column — and on the
            // last too when striping, so the stripe is a rectangle rather
            // than a ragged bar.
            if i < COLS - 1 || striped {
                text.push_str(&" ".repeat(col_widths[i].saturating_sub(cell.vis)));
            }
        }

        out.push_str(&left_margin);
        if striped {
            // Every cell's own colour wrap ends in a full reset, which would
            // clear the background mid-stripe — re-open it after each one.
            // Cell text is sanitized, so the only resets are our own.
            out.push_str(stripe);
            out.push_str(&text.replace(ansi::RESET, &format!("{}{stripe}", ansi::RESET)));
            out.push_str(ansi::RESET);
        } else {
            out.push_str(&text);
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::FailureSnapshot;

    fn failing_row(error: &str) -> CacheRow {
        CacheRow {
            shadowed: false,
            provider: "p".into(),
            path: None,
            source: "s".into(),
            field: "f".into(),
            value: serde_json::Value::Null,
            age_ms: None,
            kind: None,
            poll_interval_secs: None,
            poll_count: None,
            polls_elapsed: None,
            next_poll_in_secs: None,
            failure: Some(FailureSnapshot {
                consecutive_failures: 1,
                suppressed_until_unix_ms: None,
                last_error: Some(error.into()),
            }),
        }
    }

    #[test]
    fn a_failing_rows_error_shows_its_first_line_cut_to_length() {
        let error = format!("{}\nsecond line", "x".repeat(500));
        let out = render_table(&[failing_row(&error)], &[], false, None, false);
        let cut = format!("{}\u{2026}", "x".repeat(LAST_ERROR_CHARS - 1));
        assert!(out.contains(&cut), "{out}");
        assert!(!out.contains(&"x".repeat(LAST_ERROR_CHARS)), "{out}");
        assert!(!out.contains("second line"), "{out}");
    }

    #[test]
    fn a_failing_rows_error_fits_the_max_width() {
        let error = format!("{}\nsecond line", "x".repeat(500));
        let out = render_table(&[failing_row(&error)], &[], false, Some(80), false);
        for line in out.lines() {
            assert!(line.cols() <= 80, "{} cols: {line}", line.cols());
        }
        assert!(!out.contains("second line"), "{out}");
    }
}
